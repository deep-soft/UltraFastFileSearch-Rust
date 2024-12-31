use UltraFastFileSearch_library::config::cli_args;
// use UltraFastFileSearch_library::modules::utils::utils_impl::hello;

fn main() {
    // Parse the CLI arguments
    let cli_args = cli_args::parse_cli();

    // You can now use the parsed CLI arguments
    println!("This is the CLI tool.");
    println!("Searching in path: {}", cli_args.search_path);

    // Call other functions from your library or modules based on CLI input
    // hello();
}
