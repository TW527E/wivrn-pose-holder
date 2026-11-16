#!/usr/bin/env bash
# Build the layer and register it as an implicit OpenXR API layer for this user.
#   ./install.sh             build + install
#   ./install.sh --uninstall remove
set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")"

data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
lib_dir="$data_home/wivrn-pose-holder"
manifest_dir="$data_home/openxr/1/api_layers/implicit.d"
manifest="$manifest_dir/XrApiLayer_pose_holder.json"

if [[ "${1:-}" == "--uninstall" ]]; then
    rm -f "$manifest" "$lib_dir/libxr_pose_holder.so"
    rmdir "$lib_dir" 2>/dev/null || true
    echo "已移除。"
    exit 0
fi

# Release 壓縮檔裡已經有編譯好的 .so；從原始碼安裝時才編譯。
if [[ -f libxr_pose_holder.so ]]; then
    built=libxr_pose_holder.so
else
    # 明確指定輸出位置，不受 CARGO_TARGET_DIR 之類的設定影響
    cargo build --release --target-dir target
    built=target/release/libxr_pose_holder.so
fi

mkdir -p "$lib_dir" "$manifest_dir"
# 先寫到暫存檔再換名字，正在執行的遊戲才不會讀到寫到一半的 .so
install -m 755 "$built" "$lib_dir/libxr_pose_holder.so.new"
mv -f "$lib_dir/libxr_pose_holder.so.new" "$lib_dir/libxr_pose_holder.so"

cat > "$manifest" <<JSON
{
    "file_format_version": "1.0.0",
    "api_layer": {
        "name": "XR_APILAYER_TW527E_pose_holder",
        "library_path": "$lib_dir/libxr_pose_holder.so",
        "api_version": "1.0",
        "implementation_version": "1",
        "description": "Holds controllers at their last valid pose when tracking is lost",
        "disable_environment": "DISABLE_XR_APILAYER_POSE_HOLDER"
    }
}
JSON

echo "已安裝："
echo "  $lib_dir/libxr_pose_holder.so"
echo "  $manifest"
echo "重開 VRChat（以及其他 OpenXR 程式）後生效。"
