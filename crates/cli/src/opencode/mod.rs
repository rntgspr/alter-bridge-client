//! OpenCode v2 integration through its local background service API; one concern per submodule.

mod mcp;
mod reconcile;
mod service;
mod session;
mod setup;

pub(crate) use mcp::status as mcp_status;
pub(crate) use reconcile::reconcile;
pub(crate) use session::{active, available, is_session_id, synthetic};
pub(crate) use setup::{configure, enabled};
