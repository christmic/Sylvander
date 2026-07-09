const builtin = @import("builtin");

/// Base application ID
pub const base_application_id = "ai.oraculo.sylvander";

/// GTK application ID
pub const application_id = switch (builtin.mode) {
    .Debug, .ReleaseSafe => base_application_id ++ "-debug",
    .ReleaseFast, .ReleaseSmall => base_application_id,
};

pub const resource_path = "/ai/oraculo/sylvander";

/// GTK object path
pub const object_path = switch (builtin.mode) {
    .Debug, .ReleaseSafe => resource_path ++ "_debug",
    .ReleaseFast, .ReleaseSmall => resource_path,
};
