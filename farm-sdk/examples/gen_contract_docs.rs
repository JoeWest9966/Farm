//! 重新生成契约文档（单一源 → 生成物）。
//!
//! ```text
//! cargo run -p farm-sdk --example gen_contract_docs
//! ```
//!
//! 写出的文件：`docs/contract/schema.json`、`docs/contract/README.md`。
//! 测试 `farm-sdk/tests/contract_docs_are_generated.rs` 断言签入文件与生成结果一致，
//! 所以文档不可能与 `farm-sdk` 类型漂移。
use std::fs;
fn main() {
    let out_dir = farm_sdk::docs::DOCS_DIR.clone();
    fs::create_dir_all(&out_dir).expect("create docs/contract");
    let rendered = farm_sdk::docs::render_all();
    for (name, content) in &rendered {
        let path = out_dir.join(name);
        fs::write(&path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        println!("wrote {} ({} bytes)", path.display(), content.len());
    }
    println!(
        "contract docs regenerated from farm-sdk {} (contract version {})",
        farm_sdk::SDK_VERSION,
        farm_sdk::CONTRACT_VERSION
    );
}
