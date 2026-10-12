//! Commands for devices: what an admin asked for, signed by the Hub, and
//! what the device made of it.

use disktree_proto::PollResponse;

use crate::AppState;
use crate::devices::Device;
use crate::error::ApiResult;

/// Everything waiting for `device`.
pub async fn pending_for(_state: &AppState, _device: &Device) -> ApiResult<PollResponse> {
    Ok(PollResponse::default())
}
