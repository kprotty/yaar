pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    yaar::runtime::Builder::new()
        .build()?
        .block_on(async {
            let addr: std::net::SocketAddr = "127.0.0.1:3000".parse().unwrap();
            let server = yaar::net::TcpListener::bind(addr).unwrap();
            println!("Listening on :3000");
            
            loop {
                let (mut client, _) = server.accept().await.unwrap();
                yaar::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let mut read_offset = 0;
                    let mut read_buffer = [0; 4096];
                    const RESPONSE: &'static [u8] = b"HTTP/1.1 200 Ok\r\nContent-Length: 10\r\nContent-Type: text/plain; charset=utf8\r\n\r\nHelloWorld";
                    const ERROR: &'static [u8] = b"HTTP/1.1 400 Bad Response\r\n\r\n";

                    loop {
                        let needle = b"\r\n\r\n";
                        while let Some(parsed) = read_buffer[..read_offset].windows(needle.len()).position(|window| window == needle) {
                            let parsed = parsed + needle.len();
                            read_buffer.copy_within(parsed.., 0);
                            read_offset -= parsed;
                            client.write_all(RESPONSE).await?;
                        }

                        let read_buf = &mut read_buffer[read_offset..];
                        if read_buf.len() == 0 {
                            client.write_all(ERROR).await?;
                            return Ok(());
                        }

                        match client.read(read_buf).await {
                            Ok(0) => return Ok(()),
                            Ok(n) => read_offset += n,
                            Err(e) => return Err(e),
                        }
                    }       
                });
            }
        });
    Ok(())
}