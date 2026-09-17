fn main() {
    match local_sensitive_store_desktop_lib::recovery_tool::run(std::env::args().skip(1).collect()) {
        Ok(value) => println!("{value}"),
        Err(error) => { eprintln!("{error}"); std::process::exit(1); }
    }
}
