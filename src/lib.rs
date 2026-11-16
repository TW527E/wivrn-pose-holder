//! OpenXR API layer that freezes controllers at their last valid pose when
//! tracking is lost, instead of letting the application see them vanish.
//!
//! Linux / WiVRn counterpart of <https://github.com/z3y/driver-pose-holder>.
//! That one is a SteamVR driver hooking `TrackedDevicePoseUpdated`; without
//! SteamVR the equivalent choke point is `xrLocateSpace` on action spaces,
//! between the application (xrizer, for OpenVR games) and the runtime.

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::missing_safety_doc)]

pub mod hold;

use std::{
    collections::HashMap,
    ffi::{CStr, c_char},
    mem::{size_of, transmute},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::Instant,
};

use hold::{Config, Decision, Holder};
use openxr_sys as xr;
use xr::{Handle, pfn};

pub const LAYER_NAME: &CStr = c"XR_APILAYER_TW527E_pose_holder";

/// The next layer's (or the runtime's) entry points for one instance.
struct Dispatch {
    get_instance_proc_addr: pfn::GetInstanceProcAddr,
    destroy_instance: pfn::DestroyInstance,
    create_session: pfn::CreateSession,
    destroy_session: pfn::DestroySession,
    create_action_space: pfn::CreateActionSpace,
    destroy_space: pfn::DestroySpace,
    locate_space: pfn::LocateSpace,
    get_current_interaction_profile: pfn::GetCurrentInteractionProfile,
    get_action_state_pose: pfn::GetActionStatePose,
    poll_event: pfn::PollEvent,
}

#[derive(Clone, Copy)]
struct HeldPose {
    flags: xr::SpaceLocationFlags,
    pose: xr::Posef,
}

/// What an action space tracks. xrizer destroys and recreates its controller
/// spaces whenever the runtime sends an interaction-profile-changed event,
/// which WiVRn does exactly when a controller goes idle. Holding state is
/// therefore keyed by what the space *is*, not by its handle, so a recreated
/// space picks up the pose its predecessor was holding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct SpaceKey {
    session: u64,
    action: u64,
    subaction_path: u64,
    /// `pose_in_action_space`, bit for bit.
    offset: [u32; 7],
}

impl SpaceKey {
    fn new(session: xr::Session, info: &xr::ActionSpaceCreateInfo) -> Self {
        let p = &info.pose_in_action_space;
        Self {
            session: session.into_raw(),
            action: info.action.into_raw(),
            subaction_path: info.subaction_path.into_raw(),
            offset: [
                p.orientation.x,
                p.orientation.y,
                p.orientation.z,
                p.orientation.w,
                p.position.x,
                p.position.y,
                p.position.z,
            ]
            .map(f32::to_bits),
        }
    }
}

#[derive(Default)]
struct ProfileState {
    holder: Holder<xr::Path>,
    /// The profile the application was last told about, if it ever asked.
    shown: Option<xr::Path>,
}

impl ProfileState {
    /// Runs the runtime's answer through the hold logic and returns what the
    /// application should see.
    fn visible(&mut self, cfg: &Config, runtime: xr::Path) -> xr::Path {
        let reported = (runtime != xr::Path::NULL).then_some(runtime);
        match self.holder.update(cfg, 0, 0, reported, Instant::now()) {
            Decision::Hold(held) => held,
            Decision::PassThrough => runtime,
        }
    }
}

struct ActionSpace {
    dispatch: Arc<Dispatch>,
    key: SpaceKey,
}

#[derive(Default)]
struct State {
    instances: HashMap<u64, Arc<Dispatch>>,
    /// Most recently created instance; used for handles we do not track
    /// (reference spaces, extension spaces, ...).
    current: Option<Arc<Dispatch>>,
    sessions: HashMap<u64, Arc<Dispatch>>,
    action_spaces: HashMap<u64, ActionSpace>,
    /// (what is located, base space handle) -> hold state.
    poses: HashMap<(SpaceKey, u64), Holder<HeldPose>>,
    /// (session, top level user path) -> interaction profile state.
    profiles: HashMap<(u64, u64), ProfileState>,
    /// Last time `poll_event` checked whether a held profile was released.
    last_release_check: Option<Instant>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn config() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(Config::from_env)
}

fn log(msg: std::fmt::Arguments) {
    eprintln!("[pose_holder] {msg}");
}

fn ok(result: xr::Result) -> bool {
    result.into_raw() >= 0
}

// ---------------------------------------------------------------------------
// Loader negotiation and instance creation
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "system" fn xrNegotiateLoaderApiLayerInterface(
    loader_info: *const xr::NegotiateLoaderInfo,
    _layer_name: *const c_char,
    request: *mut xr::NegotiateApiLayerRequest,
) -> xr::Result {
    const INTERFACE_VERSION: u32 = 1;

    if loader_info.is_null() || request.is_null() {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }
    let info = &*loader_info;
    let req = &mut *request;
    if info.struct_type != xr::NegotiateLoaderInfo::TYPE
        || info.struct_version != xr::NegotiateLoaderInfo::VERSION
        || info.struct_size != size_of::<xr::NegotiateLoaderInfo>()
        || req.struct_type != xr::NegotiateApiLayerRequest::TYPE
        || req.struct_version != xr::NegotiateApiLayerRequest::VERSION
        || req.struct_size != size_of::<xr::NegotiateApiLayerRequest>()
        || info.min_interface_version > INTERFACE_VERSION
        || info.max_interface_version < INTERFACE_VERSION
    {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }
    // The layer only uses OpenXR 1.0 core functions, so any version the
    // loader accepts works; advertise the newest one inside its range.
    let api_version = if xr::CURRENT_API_VERSION.into_raw() <= info.max_api_version.into_raw() {
        xr::CURRENT_API_VERSION
    } else {
        info.max_api_version
    };
    if api_version.into_raw() < info.min_api_version.into_raw() {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    req.layer_interface_version = INTERFACE_VERSION;
    req.layer_api_version = api_version;
    req.get_instance_proc_addr = Some(get_instance_proc_addr);
    req.create_api_layer_instance = Some(create_api_layer_instance);
    xr::Result::SUCCESS
}

unsafe extern "system" fn create_api_layer_instance(
    info: *const xr::InstanceCreateInfo,
    layer_info: *const xr::ApiLayerCreateInfo,
    instance: *mut xr::Instance,
) -> xr::Result {
    if layer_info.is_null() || instance.is_null() || (*layer_info).next_info.is_null() {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }
    let next_info = &*(*layer_info).next_info;
    let (Some(next_gipa), Some(next_create)) = (
        next_info.next_get_instance_proc_addr,
        next_info.next_create_api_layer_instance,
    ) else {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    };

    // Hand the rest of the chain to the next layer.
    let mut next_layer_info = *layer_info;
    next_layer_info.next_info = next_info.next;
    let result = next_create(info, &next_layer_info, instance);
    if !ok(result) {
        return result;
    }

    match Dispatch::load(*instance, next_gipa) {
        Some(dispatch) => {
            let cfg = config();
            log(format_args!(
                "active (min_valid_frames={}, max_hold={}, hold_connection={})",
                cfg.min_valid_frames,
                cfg.max_hold
                    .map_or("forever".to_owned(), |d| format!("{:.1}s", d.as_secs_f64())),
                cfg.hold_connection
            ));
            let dispatch = Arc::new(dispatch);
            let mut st = state();
            st.instances
                .insert((*instance).into_raw(), dispatch.clone());
            st.current = Some(dispatch);
        }
        None => log(format_args!(
            "could not resolve runtime functions, layer disabled for this instance"
        )),
    }
    result
}

impl Dispatch {
    unsafe fn load(instance: xr::Instance, gipa: pfn::GetInstanceProcAddr) -> Option<Self> {
        // Every function pointer type has the same representation as
        // `VoidFunction`; the name decides the real signature.
        unsafe fn get<F: Copy>(
            instance: xr::Instance,
            gipa: pfn::GetInstanceProcAddr,
            name: &CStr,
        ) -> Option<F> {
            assert_eq!(size_of::<F>(), size_of::<pfn::VoidFunction>());
            let mut f: Option<pfn::VoidFunction> = None;
            if !ok(gipa(instance, name.as_ptr(), &mut f)) {
                return None;
            }
            f.map(|f| std::mem::transmute_copy(&f))
        }

        Some(Self {
            get_instance_proc_addr: gipa,
            destroy_instance: get(instance, gipa, c"xrDestroyInstance")?,
            create_session: get(instance, gipa, c"xrCreateSession")?,
            destroy_session: get(instance, gipa, c"xrDestroySession")?,
            create_action_space: get(instance, gipa, c"xrCreateActionSpace")?,
            destroy_space: get(instance, gipa, c"xrDestroySpace")?,
            locate_space: get(instance, gipa, c"xrLocateSpace")?,
            get_current_interaction_profile: get(
                instance,
                gipa,
                c"xrGetCurrentInteractionProfile",
            )?,
            get_action_state_pose: get(instance, gipa, c"xrGetActionStatePose")?,
            poll_event: get(instance, gipa, c"xrPollEvent")?,
        })
    }
}

unsafe extern "system" fn get_instance_proc_addr(
    instance: xr::Instance,
    name: *const c_char,
    function: *mut Option<pfn::VoidFunction>,
) -> xr::Result {
    if name.is_null() || function.is_null() {
        return xr::Result::ERROR_VALIDATION_FAILURE;
    }
    let dispatch = {
        let st = state();
        st.instances
            .get(&instance.into_raw())
            .or(st.current.as_ref())
            .cloned()
    };
    let Some(dispatch) = dispatch else {
        *function = None;
        return xr::Result::ERROR_HANDLE_INVALID;
    };

    // Ask the runtime first so we never expose a function it does not have.
    let result = (dispatch.get_instance_proc_addr)(instance, name, function);
    if !ok(result) || (*function).is_none() {
        return result;
    }

    macro_rules! hook {
        ($f:expr, $ty:ty) => {
            Some(transmute::<$ty, pfn::VoidFunction>($f))
        };
    }
    let ours = match CStr::from_ptr(name).to_bytes() {
        b"xrGetInstanceProcAddr" => hook!(get_instance_proc_addr, pfn::GetInstanceProcAddr),
        b"xrDestroyInstance" => hook!(destroy_instance, pfn::DestroyInstance),
        b"xrCreateSession" => hook!(create_session, pfn::CreateSession),
        b"xrDestroySession" => hook!(destroy_session, pfn::DestroySession),
        b"xrCreateActionSpace" => hook!(create_action_space, pfn::CreateActionSpace),
        b"xrDestroySpace" => hook!(destroy_space, pfn::DestroySpace),
        b"xrLocateSpace" => hook!(locate_space, pfn::LocateSpace),
        b"xrGetCurrentInteractionProfile" => hook!(
            get_current_interaction_profile,
            pfn::GetCurrentInteractionProfile
        ),
        b"xrGetActionStatePose" => hook!(get_action_state_pose, pfn::GetActionStatePose),
        b"xrPollEvent" => hook!(poll_event, pfn::PollEvent),
        _ => None,
    };
    if ours.is_some() {
        *function = ours;
    }
    result
}

// ---------------------------------------------------------------------------
// Handle bookkeeping
// ---------------------------------------------------------------------------

unsafe extern "system" fn destroy_instance(instance: xr::Instance) -> xr::Result {
    let Some(dispatch) = state().instances.get(&instance.into_raw()).cloned() else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    let result = (dispatch.destroy_instance)(instance);
    if ok(result) {
        // Destroying an instance destroys everything created from it.
        let mut guard = state();
        let st = &mut *guard;
        st.instances.remove(&instance.into_raw());
        st.action_spaces
            .retain(|_, s| !Arc::ptr_eq(&s.dispatch, &dispatch));
        let sessions = &st.sessions;
        let owned = |session: &u64| {
            sessions
                .get(session)
                .is_some_and(|d| Arc::ptr_eq(d, &dispatch))
        };
        st.profiles.retain(|k, _| !owned(&k.0));
        st.poses.retain(|k, _| !owned(&k.0.session));
        st.sessions.retain(|_, d| !Arc::ptr_eq(d, &dispatch));
        if st
            .current
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &dispatch))
        {
            st.current = None;
        }
    }
    result
}

unsafe extern "system" fn create_session(
    instance: xr::Instance,
    create_info: *const xr::SessionCreateInfo,
    session: *mut xr::Session,
) -> xr::Result {
    let Some(dispatch) = state().instances.get(&instance.into_raw()).cloned() else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    let result = (dispatch.create_session)(instance, create_info, session);
    if ok(result) && !session.is_null() {
        state().sessions.insert((*session).into_raw(), dispatch);
    }
    result
}

fn dispatch_for_session(session: xr::Session) -> Option<Arc<Dispatch>> {
    let st = state();
    st.sessions
        .get(&session.into_raw())
        .or(st.current.as_ref())
        .cloned()
}

unsafe extern "system" fn destroy_session(session: xr::Session) -> xr::Result {
    let Some(dispatch) = dispatch_for_session(session) else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    let result = (dispatch.destroy_session)(session);
    if ok(result) {
        let raw = session.into_raw();
        let mut st = state();
        st.sessions.remove(&raw);
        st.action_spaces.retain(|_, s| s.key.session != raw);
        st.profiles.retain(|k, _| k.0 != raw);
        st.poses.retain(|k, _| k.0.session != raw);
    }
    result
}

unsafe extern "system" fn create_action_space(
    session: xr::Session,
    create_info: *const xr::ActionSpaceCreateInfo,
    space: *mut xr::Space,
) -> xr::Result {
    let Some(dispatch) = dispatch_for_session(session) else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    let result = (dispatch.create_action_space)(session, create_info, space);
    if ok(result) && !space.is_null() && !create_info.is_null() {
        state().action_spaces.insert(
            (*space).into_raw(),
            ActionSpace {
                dispatch,
                key: SpaceKey::new(session, &*create_info),
            },
        );
    }
    result
}

unsafe extern "system" fn destroy_space(space: xr::Space) -> xr::Result {
    let raw = space.into_raw();
    let dispatch = {
        let st = state();
        st.action_spaces
            .get(&raw)
            .map(|s| &s.dispatch)
            .or(st.current.as_ref())
            .cloned()
    };
    let Some(dispatch) = dispatch else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    let result = (dispatch.destroy_space)(space);
    if ok(result) {
        let mut st = state();
        // The hold state of a destroyed action space is kept on purpose (see
        // `SpaceKey`). Poses recorded relative to a destroyed *base* space are
        // dropped: its handle may be reused for a space with another origin.
        st.action_spaces.remove(&raw);
        st.poses.retain(|k, _| k.1 != raw);
    }
    result
}

// ---------------------------------------------------------------------------
// The actual pose holding
// ---------------------------------------------------------------------------

unsafe extern "system" fn locate_space(
    space: xr::Space,
    base_space: xr::Space,
    time: xr::Time,
    location: *mut xr::SpaceLocation,
) -> xr::Result {
    let dispatch = {
        let st = state();
        st.action_spaces
            .get(&space.into_raw())
            .map(|s| &s.dispatch)
            .or(st.current.as_ref())
            .cloned()
    };
    let Some(dispatch) = dispatch else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };

    let result = (dispatch.locate_space)(space, base_space, time, location);
    if !ok(result) || location.is_null() {
        return result;
    }

    let mut st = state();
    // Only controller (action) spaces are held; the HMD and trackers pass through.
    let Some(key) = st.action_spaces.get(&space.into_raw()).map(|s| s.key) else {
        return result;
    };
    let holder = st.poses.entry((key, base_space.into_raw())).or_default();

    let loc = &mut *location;
    let needed =
        xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
    // An invalid pose may be left unwritten by the runtime: do not read it.
    #[allow(clippy::unnecessary_lazy_evaluations)]
    let reported = loc.location_flags.contains(needed).then(|| HeldPose {
        flags: loc.location_flags,
        pose: loc.pose,
    });

    let cfg = config();
    let was_holding = holder.is_holding();
    let decision = holder.update(
        cfg,
        cfg.min_valid_frames,
        time.as_nanos(),
        reported,
        Instant::now(),
    );
    match decision {
        Decision::Hold(held) => {
            if !was_holding {
                log(format_args!(
                    "tracking lost on {}, holding last pose",
                    label(&key)
                ));
            }
            loc.location_flags = held.flags;
            loc.pose = held.pose;
            // A frozen controller is not moving: clear any chained velocity so
            // the application does not extrapolate away from the held pose.
            let mut next = loc.next as *mut xr::BaseOutStructure;
            while !next.is_null() {
                if (*next).ty == xr::StructureType::SPACE_VELOCITY {
                    let vel = &mut *(next as *mut xr::SpaceVelocity);
                    vel.velocity_flags = xr::SpaceVelocityFlags::LINEAR_VALID
                        | xr::SpaceVelocityFlags::ANGULAR_VALID;
                    vel.linear_velocity = xr::Vector3f::default();
                    vel.angular_velocity = xr::Vector3f::default();
                }
                next = (*next).next;
            }
        }
        Decision::PassThrough => {
            if was_holding && !holder.is_holding() {
                log(format_args!("tracking recovered on {}", label(&key)));
            }
        }
    }
    result
}

fn label(key: &SpaceKey) -> String {
    format!("action {:#x} path {:#x}", key.action, key.subaction_path)
}

/// When a controller goes idle the headset reports no interaction profile at
/// all, which applications (xrizer) treat as "controller disconnected". Keep
/// reporting the last real profile so the device stays connected while held.
unsafe extern "system" fn get_current_interaction_profile(
    session: xr::Session,
    top_level_user_path: xr::Path,
    interaction_profile: *mut xr::InteractionProfileState,
) -> xr::Result {
    let Some(dispatch) = dispatch_for_session(session) else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    let result = (dispatch.get_current_interaction_profile)(
        session,
        top_level_user_path,
        interaction_profile,
    );
    let cfg = config();
    if !ok(result) || interaction_profile.is_null() || !cfg.hold_connection {
        return result;
    }

    let profile = &mut (*interaction_profile).interaction_profile;
    let mut st = state();
    let entry = st
        .profiles
        .entry((session.into_raw(), top_level_user_path.into_raw()))
        .or_default();
    *profile = entry.visible(cfg, *profile);
    entry.shown = Some(*profile);
    result
}

/// Asks the runtime for every profile the application has looked at in
/// `session` and returns whether any of them would now look different to the
/// application. `None` when the application never asked or a query failed.
unsafe fn profiles_changed(dispatch: &Dispatch, session: u64) -> Option<bool> {
    let paths: Vec<u64> = state()
        .profiles
        .iter()
        .filter(|(k, p)| k.0 == session && p.shown.is_some())
        .map(|(k, _)| k.1)
        .collect();
    if paths.is_empty() {
        return None;
    }
    let cfg = config();
    let mut changed = false;
    for path in paths {
        let mut answer = xr::InteractionProfileState {
            ty: xr::InteractionProfileState::TYPE,
            next: std::ptr::null_mut(),
            interaction_profile: xr::Path::NULL,
        };
        // Called without the lock: the runtime may call back into the layer.
        let result = (dispatch.get_current_interaction_profile)(
            xr::Session::from_raw(session),
            xr::Path::from_raw(path),
            &mut answer,
        );
        if !ok(result) {
            return None;
        }
        let mut st = state();
        let entry = st.profiles.get_mut(&(session, path))?;
        let visible = entry.visible(cfg, answer.interaction_profile);
        changed |= entry.shown != Some(visible);
    }
    Some(changed)
}

/// WiVRn sends an interaction-profile-changed event whenever a controller goes
/// idle or wakes up. xrizer answers every such event by rebuilding all its
/// controller spaces and generic trackers (the Rebocap ones included). While a
/// controller is held the profile the application sees does not change, so
/// these events are swallowed. Real changes (controllers <-> hand tracking)
/// still get through.
unsafe extern "system" fn poll_event(
    instance: xr::Instance,
    event: *mut xr::EventDataBuffer,
) -> xr::Result {
    let Some(dispatch) = state().instances.get(&instance.into_raw()).cloned() else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    let cfg = config();
    if event.is_null() || !cfg.hold_connection {
        return (dispatch.poll_event)(instance, event);
    }
    let next = (*event).next;

    loop {
        let result = (dispatch.poll_event)(instance, event);

        if result == xr::Result::EVENT_UNAVAILABLE && cfg.max_hold.is_some() {
            // An event we swallowed earlier can leave the application believing
            // a controller is still there after the hold runs out. Tell it.
            return synthesize_release_event(&dispatch, event).unwrap_or(result);
        }
        if result != xr::Result::SUCCESS
            || (*event).ty != xr::StructureType::EVENT_DATA_INTERACTION_PROFILE_CHANGED
        {
            return result;
        }

        let session = (*(event as *const xr::EventDataInteractionProfileChanged))
            .session
            .into_raw();
        if profiles_changed(&dispatch, session) != Some(false) {
            return result;
        }
        log(format_args!(
            "ignored an interaction profile change that the application would not see"
        ));
        // Get the next event instead, into a buffer reset for reuse.
        (*event).ty = xr::EventDataBuffer::TYPE;
        (*event).next = next;
    }
}

unsafe fn synthesize_release_event(
    dispatch: &Arc<Dispatch>,
    event: *mut xr::EventDataBuffer,
) -> Option<xr::Result> {
    let sessions: Vec<u64> = {
        let mut st = state();
        let now = Instant::now();
        // The check costs a runtime query per hand, so not every frame.
        if st
            .last_release_check
            .is_some_and(|t| now.duration_since(t).as_millis() < 250)
        {
            return None;
        }
        st.last_release_check = Some(now);
        let mut sessions: Vec<u64> = st
            .profiles
            .keys()
            .map(|k| k.0)
            .filter(|s| st.sessions.get(s).is_some_and(|d| Arc::ptr_eq(d, dispatch)))
            .collect();
        sessions.sort_unstable();
        sessions.dedup();
        sessions
    };
    for session in sessions {
        if profiles_changed(dispatch, session) == Some(true) {
            log(format_args!("hold ran out, telling the application"));
            let out = event as *mut xr::EventDataInteractionProfileChanged;
            (*out).ty = xr::EventDataInteractionProfileChanged::TYPE;
            (*out).next = std::ptr::null();
            (*out).session = xr::Session::from_raw(session);
            return Some(xr::Result::SUCCESS);
        }
    }
    None
}

/// Same idea for the pose action itself: while one of its spaces is being
/// held, report the action as active. When nothing is held the runtime's answer
/// stands, so an action the application deactivated on purpose stays inactive.
unsafe extern "system" fn get_action_state_pose(
    session: xr::Session,
    get_info: *const xr::ActionStateGetInfo,
    action_state: *mut xr::ActionStatePose,
) -> xr::Result {
    let Some(dispatch) = dispatch_for_session(session) else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    let result = (dispatch.get_action_state_pose)(session, get_info, action_state);
    if !ok(result)
        || get_info.is_null()
        || action_state.is_null()
        || !config().hold_connection
        || (*action_state).is_active != xr::FALSE
    {
        return result;
    }

    let session = session.into_raw();
    let action = (*get_info).action.into_raw();
    let subaction_path = (*get_info).subaction_path.into_raw();
    let held = state().poses.iter().any(|((k, _), holder)| {
        k.session == session
            && k.action == action
            && (subaction_path == xr::Path::NULL.into_raw() || k.subaction_path == subaction_path)
            && holder.is_holding()
    });
    if held {
        (*action_state).is_active = xr::TRUE;
    }
    result
}
