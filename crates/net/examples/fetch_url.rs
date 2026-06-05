//! Example: fetch a URL and print the first 200 bytes of the body.
//!
//! Run with:
//! ```sh
//! cargo run -p browser-net --example fetch_url -- https://example.com
//! ```

use browser_net::get;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: fetch_url <url>");
        std::process::exit(2);
    }
    let url = &args[0];
    let body = get(url).await?;
    let preview: Vec<u8> = body.iter().take(200).copied().collect();
    println!("{}", String::from_utf8_lossy(&preview));
    Ok(())
}
