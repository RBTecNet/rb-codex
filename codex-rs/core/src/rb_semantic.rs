//! Static protocol vocabulary for the RB bounded semantic runtime.
//!
//! Dynamic evidence such as model identity, tool manifests, and reroutes must
//! remain derived from the effective runtime request rather than this module.

macro_rules! mode_version {
    () => {
        "v1"
    };
}
macro_rules! rb_version {
    () => {
        "0.151.0-rb.1"
    };
}
macro_rules! upstream_commit {
    () => {
        "78c290807ce710180111df227df3b7a4fe845452"
    };
}
macro_rules! binary_name {
    () => {
        "rb-codex"
    };
}

pub const MODE_VERSION: &str = mode_version!();
pub const CLI_VERSION: &str = concat!(
    rb_version!(),
    " (upstream ",
    upstream_commit!(),
    "; semantic-mode ",
    mode_version!(),
    ")"
);
pub const RUNTIME_VERSION: &str = concat!(
    binary_name!(),
    " ",
    rb_version!(),
    " (upstream ",
    upstream_commit!(),
    ")"
);
pub const BINARY_NAME: &str = binary_name!();
pub const TOOL_POLICY_NONE: &str = "none";
pub const INSTRUCTION_POLICY_ISOLATED: &str = "isolated";
pub const SESSION_MODE_EPHEMERAL: &str = "ephemeral";
pub const REQUEST_ACCOUNTING_OPAQUE: &str = "opaque";
pub const REQUESTED_CODEX_TURNS: u32 = 1;
pub const AUTO_COMPACTION_REJECTED: &str = "RB semantic mode does not permit automatic compaction";
