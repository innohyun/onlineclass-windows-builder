#[tokio::main]
async fn main() {
    if let Err(error) = local_sensitive_store_desktop_lib::run_student_record_mcp_stdio().await {
        eprintln!("classaimate-student-record-mcp: {error}");
        std::process::exit(1);
    }
}
