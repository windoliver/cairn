//! User code outside `cairn_core::wal` must not be able to mint an
//! [`ApplyToken`] via a struct literal — the private `_private` field
//! blocks construction at compile time.

use cairn_core::contract::memory_store::apply::ApplyToken;

fn main() {
    let _t = ApplyToken { _private: () };
}
