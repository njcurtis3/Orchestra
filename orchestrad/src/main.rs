fn main() {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("orchestrad runs on Linux");
        std::process::exit(1);
    }
}
