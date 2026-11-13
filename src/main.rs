fn main() {
    if let Err(err) = bumpkin::parse(std::env::args().skip(1)).and_then(bumpkin::run) {
        eprintln!("ERROR {err}");
        std::process::exit(1);
    }
}
