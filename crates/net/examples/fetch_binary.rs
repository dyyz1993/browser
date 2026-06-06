use browser_net::HttpClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = HttpClient::new();
    let url = "https://via.placeholder.com/150";
    let bytes = client.get(url).await?;
    println!("Fetched {} bytes from {}", bytes.len(), url);
    println!("First 10 bytes: {:?}", &bytes[..bytes.len().min(10)]);
    Ok(())
}
