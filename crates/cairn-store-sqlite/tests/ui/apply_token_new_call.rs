//! User code outside `cairn_core::wal` must not be able to call the
//! private `ApplyToken::new()` constructor.

use cairn_core::contract::memory_store::apply::ApplyToken;

fn main() {
    let _t = ApplyToken::new();
}
