use crabgent_core::{ReadFileTool, Subject, Tool, ToolCtx};
use serde_json::json;
use tempfile::tempdir;

const FILE_SIZE: u64 = 100 * 1024 * 1024;
const CAP: usize = 4 * 1024;
const TRUNCATE_MARKER: &str = "\n... [truncated]";

#[tokio::test]
async fn read_file_uses_bounded_cap_plus_one_read() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("large.bin");
    let file = std::fs::File::create(&path).expect("create sparse file");
    file.set_len(FILE_SIZE).expect("size sparse file");
    drop(file);

    let tool = ReadFileTool::without_root().with_max_bytes(CAP as u64);
    let out = tool
        .execute(
            json!({"path": path.to_str().expect("utf8 path")}),
            &ToolCtx::new(Subject::new("u")),
        )
        .await
        .expect("read file");

    let content = out["content"].as_str().expect("content");
    assert!(out["truncated"].as_bool().expect("truncated"));
    assert_eq!(out["size_bytes"], FILE_SIZE);
    assert!(content.len() <= CAP + TRUNCATE_MARKER.len());
    assert!(content.ends_with(TRUNCATE_MARKER));
}
