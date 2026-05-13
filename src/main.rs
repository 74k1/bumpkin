fn main() {
    if let Err(err) = bumpkin::run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
