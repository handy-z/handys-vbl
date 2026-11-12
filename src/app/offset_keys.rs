use std::sync::OnceLock;

use handy_sdk::roblox::events::CHAT_KEYS;
use handy_sdk::roblox::offsets::OffsetKeys;
use handy_sdk::roblox::read::join_keys;

/// Every offset vbl reads (a superset of SDK_KEYS and CHAT_KEYS); offsets missing any are rejected.
pub fn required_keys() -> &'static OffsetKeys {
    static KEYS: OnceLock<OffsetKeys> = OnceLock::new();
    KEYS.get_or_init(|| {
        join_keys(
            &[&CHAT_KEYS],
            &[
                "FakeDataModel.Pointer",
                "FakeDataModel.RealDataModel",
                "DataModel.Workspace",
                "Workspace.CurrentCamera",
                "Camera.CameraSubject",
                "Camera.Rotation",
                "Camera.Position",
                "Instance.Parent",
                "Instance.NameContainer",
                "Instance.ClassDescriptor",
                "Instance.ClassName",
                "Instance.ChildrenStart",
                "Instance.ChildrenEnd",
                "Model.PrimaryPart",
                "BasePart.Primitive",
                "Primitive.Position",
                "Primitive.Rotation",
                "Primitive.Size",
                "Player.ModelInstance",
                "Player.LocalPlayer",
                "GuiObject.Visible",
                "GuiObject.Text",
                "GuiObject.Image",
                "Misc.StringLength",
            ],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use handy_sdk::roblox::offsets::SDK_KEYS;

    #[test]
    fn offset_keys_cover_sdk_keys() {
        let keys = required_keys();
        for k in SDK_KEYS {
            assert!(keys.contains(&k), "{k}");
        }
        for k in CHAT_KEYS {
            assert!(keys.contains(&k), "{k}");
        }
    }
}
