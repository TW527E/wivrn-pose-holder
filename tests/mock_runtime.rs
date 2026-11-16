//! Drives the layer the way the OpenXR loader does, against a fake runtime,
//! so the whole negotiate -> create instance -> hooked call path is covered
//! without a headset.

#![allow(unsafe_op_in_unsafe_fn)]

use std::{
    ffi::{CStr, c_char},
    mem::{size_of, transmute, zeroed},
    ptr,
    sync::Mutex,
};

use openxr_sys as xr;
use xr::{Handle, pfn};
use xr_pose_holder::xrNegotiateLoaderApiLayerInterface;

/// What the fake runtime answers on the next call.
struct Runtime {
    tracked: bool,
    x: f32,
    profile: u64,
    pose_active: bool,
    /// Pending events, oldest first.
    events: Vec<xr::StructureType>,
}

static RUNTIME: Mutex<Runtime> = Mutex::new(Runtime {
    tracked: true,
    x: 0.0,
    profile: 42,
    pose_active: true,
    events: Vec::new(),
});

const ACTION_SPACE: u64 = 0x100;
/// What the fake runtime hands out when the application recreates the space.
const RECREATED_SPACE: u64 = 0x101;
const REFERENCE_SPACE: u64 = 0x200;

unsafe extern "system" fn rt_create_instance(
    _info: *const xr::InstanceCreateInfo,
    _layer_info: *const xr::ApiLayerCreateInfo,
    instance: *mut xr::Instance,
) -> xr::Result {
    *instance = xr::Instance::from_raw(1);
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_destroy_instance(_: xr::Instance) -> xr::Result {
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_create_session(
    _: xr::Instance,
    _: *const xr::SessionCreateInfo,
    session: *mut xr::Session,
) -> xr::Result {
    *session = xr::Session::from_raw(7);
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_destroy_session(_: xr::Session) -> xr::Result {
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_create_action_space(
    _: xr::Session,
    _: *const xr::ActionSpaceCreateInfo,
    space: *mut xr::Space,
) -> xr::Result {
    // The first space gets ACTION_SPACE, later ones RECREATED_SPACE.
    static CREATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let again = CREATED.swap(true, std::sync::atomic::Ordering::Relaxed);
    *space = xr::Space::from_raw(if again { RECREATED_SPACE } else { ACTION_SPACE });
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_destroy_space(_: xr::Space) -> xr::Result {
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_locate_space(
    _space: xr::Space,
    _base: xr::Space,
    _time: xr::Time,
    location: *mut xr::SpaceLocation,
) -> xr::Result {
    let rt = RUNTIME.lock().unwrap();
    let loc = &mut *location;
    let vel = loc.next as *mut xr::SpaceVelocity;
    if rt.tracked {
        loc.location_flags = xr::SpaceLocationFlags::POSITION_VALID
            | xr::SpaceLocationFlags::POSITION_TRACKED
            | xr::SpaceLocationFlags::ORIENTATION_VALID
            | xr::SpaceLocationFlags::ORIENTATION_TRACKED;
        loc.pose.position.x = rt.x;
        loc.pose.orientation.w = 1.0;
        if !vel.is_null() {
            (*vel).velocity_flags = xr::SpaceVelocityFlags::LINEAR_VALID;
            (*vel).linear_velocity.x = 3.0;
        }
    } else {
        loc.location_flags = xr::SpaceLocationFlags::EMPTY;
        loc.pose = zeroed();
        if !vel.is_null() {
            (*vel).velocity_flags = xr::SpaceVelocityFlags::EMPTY;
        }
    }
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_get_profile(
    _: xr::Session,
    _: xr::Path,
    state: *mut xr::InteractionProfileState,
) -> xr::Result {
    (*state).interaction_profile = xr::Path::from_raw(RUNTIME.lock().unwrap().profile);
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_get_pose_state(
    _: xr::Session,
    _: *const xr::ActionStateGetInfo,
    state: *mut xr::ActionStatePose,
) -> xr::Result {
    (*state).is_active = RUNTIME.lock().unwrap().pose_active.into();
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_poll_event(
    _: xr::Instance,
    event: *mut xr::EventDataBuffer,
) -> xr::Result {
    let mut rt = RUNTIME.lock().unwrap();
    if rt.events.is_empty() {
        return xr::Result::EVENT_UNAVAILABLE;
    }
    // Like a real runtime: refuse a buffer that was not reset for reuse.
    assert_eq!((*event).ty, xr::EventDataBuffer::TYPE);
    let ty = rt.events.remove(0);
    if ty == xr::EventDataInteractionProfileChanged::TYPE {
        let out = event as *mut xr::EventDataInteractionProfileChanged;
        (*out).ty = ty;
        (*out).session = xr::Session::from_raw(7);
    } else {
        (*event).ty = ty;
    }
    xr::Result::SUCCESS
}

unsafe extern "system" fn rt_untouched() {}

unsafe extern "system" fn rt_gipa(
    _instance: xr::Instance,
    name: *const c_char,
    function: *mut Option<pfn::VoidFunction>,
) -> xr::Result {
    macro_rules! f {
        ($f:expr, $ty:ty) => {
            Some(transmute::<$ty, pfn::VoidFunction>($f))
        };
    }
    *function = match CStr::from_ptr(name).to_bytes() {
        b"xrDestroyInstance" => f!(rt_destroy_instance, pfn::DestroyInstance),
        b"xrCreateSession" => f!(rt_create_session, pfn::CreateSession),
        b"xrDestroySession" => f!(rt_destroy_session, pfn::DestroySession),
        b"xrCreateActionSpace" => f!(rt_create_action_space, pfn::CreateActionSpace),
        b"xrDestroySpace" => f!(rt_destroy_space, pfn::DestroySpace),
        b"xrLocateSpace" => f!(rt_locate_space, pfn::LocateSpace),
        b"xrGetCurrentInteractionProfile" => {
            f!(rt_get_profile, pfn::GetCurrentInteractionProfile)
        }
        b"xrGetActionStatePose" => f!(rt_get_pose_state, pfn::GetActionStatePose),
        b"xrPollEvent" => f!(rt_poll_event, pfn::PollEvent),
        b"xrEndFrame" => Some(rt_untouched as pfn::VoidFunction),
        _ => return xr::Result::ERROR_FUNCTION_UNSUPPORTED,
    };
    xr::Result::SUCCESS
}

struct Layer {
    instance: xr::Instance,
    gipa: pfn::GetInstanceProcAddr,
}

impl Layer {
    unsafe fn load() -> Self {
        let loader_info = xr::NegotiateLoaderInfo {
            struct_type: xr::NegotiateLoaderInfo::TYPE,
            struct_version: xr::NegotiateLoaderInfo::VERSION,
            struct_size: size_of::<xr::NegotiateLoaderInfo>(),
            min_interface_version: 1,
            max_interface_version: 1,
            min_api_version: xr::Version::new(1, 0, 0),
            max_api_version: xr::Version::new(1, 1, 1023),
        };
        let mut request = xr::NegotiateApiLayerRequest {
            struct_type: xr::NegotiateApiLayerRequest::TYPE,
            struct_version: xr::NegotiateApiLayerRequest::VERSION,
            struct_size: size_of::<xr::NegotiateApiLayerRequest>(),
            layer_interface_version: 0,
            layer_api_version: xr::Version::new(0, 0, 0),
            get_instance_proc_addr: None,
            create_api_layer_instance: None,
        };
        let name = xr_pose_holder::LAYER_NAME;
        let r = xrNegotiateLoaderApiLayerInterface(&loader_info, name.as_ptr(), &mut request);
        assert_eq!(r, xr::Result::SUCCESS);

        let mut next_info: xr::ApiLayerNextInfo = zeroed();
        next_info.struct_type = xr::ApiLayerNextInfo::TYPE;
        next_info.struct_version = xr::ApiLayerNextInfo::VERSION;
        next_info.struct_size = size_of::<xr::ApiLayerNextInfo>();
        next_info.next_get_instance_proc_addr = Some(rt_gipa);
        next_info.next_create_api_layer_instance = Some(rt_create_instance);
        let mut layer_info: xr::ApiLayerCreateInfo = zeroed();
        layer_info.struct_type = xr::ApiLayerCreateInfo::TYPE;
        layer_info.struct_version = xr::ApiLayerCreateInfo::VERSION;
        layer_info.struct_size = size_of::<xr::ApiLayerCreateInfo>();
        layer_info.next_info = &mut next_info;

        let mut instance = xr::Instance::NULL;
        let create = request.create_api_layer_instance.unwrap();
        assert_eq!(
            create(ptr::null(), &layer_info, &mut instance),
            xr::Result::SUCCESS
        );
        Self {
            instance,
            gipa: request.get_instance_proc_addr.unwrap(),
        }
    }

    unsafe fn get<F: Copy>(&self, name: &CStr) -> F {
        let mut f = None;
        assert_eq!(
            (self.gipa)(self.instance, name.as_ptr(), &mut f),
            xr::Result::SUCCESS
        );
        std::mem::transmute_copy(&f.unwrap())
    }
}

struct Located {
    flags: xr::SpaceLocationFlags,
    x: f32,
    vel_x: f32,
    vel_flags: xr::SpaceVelocityFlags,
}

unsafe fn locate(f: pfn::LocateSpace, space: u64, frame: i64) -> Located {
    let mut vel: xr::SpaceVelocity = zeroed();
    vel.ty = xr::StructureType::SPACE_VELOCITY;
    let mut loc: xr::SpaceLocation = zeroed();
    loc.ty = xr::StructureType::SPACE_LOCATION;
    loc.next = &mut vel as *mut _ as *mut _;
    let r = f(
        xr::Space::from_raw(space),
        xr::Space::from_raw(REFERENCE_SPACE),
        xr::Time::from_nanos(frame),
        &mut loc,
    );
    assert_eq!(r, xr::Result::SUCCESS);
    Located {
        flags: loc.location_flags,
        x: loc.pose.position.x,
        vel_x: vel.linear_velocity.x,
        vel_flags: vel.velocity_flags,
    }
}

fn set(f: impl FnOnce(&mut Runtime)) {
    f(&mut RUNTIME.lock().unwrap());
}

#[test]
fn layer_holds_controller_through_tracking_loss() {
    unsafe {
        let layer = Layer::load();

        // Functions we do not hook must reach the runtime untouched, and
        // ones the runtime lacks must stay unavailable.
        let end_frame: pfn::VoidFunction = layer.get(c"xrEndFrame");
        assert_eq!(end_frame as usize, rt_untouched as *const () as usize);
        let mut missing = None;
        assert_eq!(
            (layer.gipa)(layer.instance, c"xrLocateSpaces".as_ptr(), &mut missing),
            xr::Result::ERROR_FUNCTION_UNSUPPORTED
        );

        let create_session: pfn::CreateSession = layer.get(c"xrCreateSession");
        let create_action_space: pfn::CreateActionSpace = layer.get(c"xrCreateActionSpace");
        let locate_space: pfn::LocateSpace = layer.get(c"xrLocateSpace");
        let get_profile: pfn::GetCurrentInteractionProfile =
            layer.get(c"xrGetCurrentInteractionProfile");
        let get_pose_state: pfn::GetActionStatePose = layer.get(c"xrGetActionStatePose");
        let poll_event: pfn::PollEvent = layer.get(c"xrPollEvent");
        let poll = || {
            let mut event: xr::EventDataBuffer = zeroed();
            event.ty = xr::EventDataBuffer::TYPE;
            match poll_event(layer.instance, &mut event) {
                xr::Result::SUCCESS => Some(event.ty),
                xr::Result::EVENT_UNAVAILABLE => None,
                other => panic!("xrPollEvent returned {other:?}"),
            }
        };
        let profile_changed = xr::EventDataInteractionProfileChanged::TYPE;
        let session_state = xr::StructureType::EVENT_DATA_SESSION_STATE_CHANGED;
        let destroy_space: pfn::DestroySpace = layer.get(c"xrDestroySpace");

        let mut session = xr::Session::NULL;
        assert_eq!(
            create_session(layer.instance, ptr::null(), &mut session),
            xr::Result::SUCCESS
        );
        let create_info: xr::ActionSpaceCreateInfo = zeroed();
        let mut space = xr::Space::NULL;
        assert_eq!(
            create_action_space(session, &create_info, &mut space),
            xr::Result::SUCCESS
        );
        assert_eq!(space.into_raw(), ACTION_SPACE);

        let profile = || {
            let mut state: xr::InteractionProfileState = zeroed();
            get_profile(session, xr::Path::from_raw(5), &mut state);
            state.interaction_profile.into_raw()
        };
        let pose_active = || {
            let info: xr::ActionStateGetInfo = zeroed();
            let mut state: xr::ActionStatePose = zeroed();
            get_pose_state(session, &info, &mut state);
            state.is_active != xr::FALSE
        };

        // Tracked: everything passes through, velocity included.
        for frame in 1..=10 {
            set(|rt| rt.x = frame as f32);
            let l = locate(locate_space, ACTION_SPACE, frame);
            assert_eq!(l.x, frame as f32);
            assert_eq!(l.vel_x, 3.0);
        }
        assert_eq!(profile(), 42);
        assert!(pose_active());

        // Controller goes idle: no pose, no profile, pose action inactive.
        set(|rt| {
            rt.tracked = false;
            rt.profile = 0;
            rt.pose_active = false;
        });
        for frame in 11..=20 {
            let l = locate(locate_space, ACTION_SPACE, frame);
            assert!(l.flags.contains(
                xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID
            ));
            assert_eq!(l.x, 10.0, "held at the last tracked pose");
            assert_eq!(l.vel_x, 0.0, "a held controller does not move");
            assert!(l.vel_flags.contains(xr::SpaceVelocityFlags::LINEAR_VALID));
        }
        assert_eq!(profile(), 42, "controller stays connected");

        // WiVRn announces the idle controller with a profile-changed event.
        // The application would see no change, so it never gets the event
        // (xrizer would rebuild every tracker); other events still arrive.
        set(|rt| rt.events = vec![profile_changed, session_state, profile_changed]);
        assert_eq!(poll(), Some(session_state));
        assert_eq!(poll(), None);
        assert!(pose_active());

        // What xrizer does on the profile-changed event WiVRn sends when the
        // controller goes idle: destroy the controller space and create a new
        // one for the same action. The new space must keep holding.
        assert_eq!(destroy_space(space), xr::Result::SUCCESS);
        let mut space = xr::Space::NULL;
        assert_eq!(
            create_action_space(session, &create_info, &mut space),
            xr::Result::SUCCESS
        );
        assert_eq!(space.into_raw(), RECREATED_SPACE);
        let l = locate(locate_space, RECREATED_SPACE, 21);
        assert_eq!(l.x, 10.0, "recreated space still held");
        assert!(pose_active());

        // A space the layer did not see created (HMD, trackers) is never held.
        let other = locate(locate_space, REFERENCE_SPACE, 20);
        assert!(other.flags.is_empty());

        // Tracking returns at a bogus pose: the first 5 frames are ignored.
        set(|rt| {
            rt.tracked = true;
            rt.profile = 42;
            rt.pose_active = true;
            rt.x = 99.0;
        });
        // The wake-up event changes nothing for the application either.
        set(|rt| rt.events = vec![profile_changed]);
        assert_eq!(poll(), None);
        for frame in 22..=26 {
            assert_eq!(locate(locate_space, RECREATED_SPACE, frame).x, 10.0);
        }
        set(|rt| rt.x = 11.0);
        let l = locate(locate_space, RECREATED_SPACE, 27);
        assert_eq!(l.x, 11.0);
        assert_eq!(l.vel_x, 3.0);

        // Nothing is held now, so an action the application deactivated
        // itself is reported as the runtime says.
        set(|rt| rt.pose_active = false);
        assert!(!pose_active());
        set(|rt| rt.pose_active = true);

        // Switching to another real profile (e.g. hand tracking) is not blocked.
        set(|rt| {
            rt.profile = 43;
            rt.events = vec![profile_changed];
        });
        assert_eq!(poll(), Some(profile_changed), "a real change gets through");
        assert_eq!(profile(), 43);

        // Once the base space is destroyed, poses recorded against it are gone.
        assert_eq!(
            destroy_space(xr::Space::from_raw(REFERENCE_SPACE)),
            xr::Result::SUCCESS
        );
        set(|rt| rt.tracked = false);
        assert!(locate(locate_space, RECREATED_SPACE, 28).flags.is_empty());
    }
}

#[test]
fn negotiation_stays_inside_the_loaders_api_range() {
    unsafe {
        let mut loader_info = xr::NegotiateLoaderInfo {
            struct_type: xr::NegotiateLoaderInfo::TYPE,
            struct_version: xr::NegotiateLoaderInfo::VERSION,
            struct_size: size_of::<xr::NegotiateLoaderInfo>(),
            min_interface_version: 1,
            max_interface_version: 1,
            min_api_version: xr::Version::new(1, 0, 0),
            max_api_version: xr::Version::new(1, 0, 1023),
        };
        let mut request: xr::NegotiateApiLayerRequest = zeroed();
        request.struct_type = xr::NegotiateApiLayerRequest::TYPE;
        request.struct_version = xr::NegotiateApiLayerRequest::VERSION;
        request.struct_size = size_of::<xr::NegotiateApiLayerRequest>();
        let name = xr_pose_holder::LAYER_NAME.as_ptr();

        // A loader limited to OpenXR 1.0 gets a 1.0 answer.
        assert_eq!(
            xrNegotiateLoaderApiLayerInterface(&loader_info, name, &mut request),
            xr::Result::SUCCESS
        );
        assert_eq!(request.layer_api_version.major(), 1);
        assert_eq!(request.layer_api_version.minor(), 0);

        // A loader that only accepts something newer than we know is refused.
        loader_info.min_api_version = xr::Version::new(9, 0, 0);
        loader_info.max_api_version = xr::Version::new(9, 9, 0);
        assert_eq!(
            xrNegotiateLoaderApiLayerInterface(&loader_info, name, &mut request),
            xr::Result::ERROR_INITIALIZATION_FAILED
        );
    }
}
