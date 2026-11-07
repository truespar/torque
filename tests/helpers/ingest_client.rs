use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use torque::ingest::protocol::*;
use torque::schema::{Document, Schema};

pub struct TestIngestClient {
    stream: TcpStream,
}

impl TestIngestClient {
    pub async fn connect(addr: &str) -> Self {
        let stream = TcpStream::connect(addr).await.expect("failed to connect");
        Self { stream }
    }

    pub async fn start_ingest(&mut self, collection: &str) -> ServerMessage {
        let msg = ClientMessage::StartIngest {
            collection: collection.into(),
            mode: 0,
        };
        self.send_and_receive(&msg).await
    }

    pub async fn send_batch(&mut self, docs: &[Document], schema: &Schema) -> ServerMessage {
        let msg = encode_doc_batch(docs, schema);
        self.send_and_receive(&msg).await
    }

    pub async fn commit(&mut self) -> ServerMessage {
        self.send_and_receive(&ClientMessage::Commit).await
    }

    pub async fn abort(&mut self) -> ServerMessage {
        self.send_and_receive(&ClientMessage::Abort).await
    }

    async fn send_and_receive(&mut self, msg: &ClientMessage) -> ServerMessage {
        let data = serialize_client_message(msg);
        self.write_frame(&data).await;
        let response_data = self.read_frame().await;
        deserialize_server_message(&response_data).expect("failed to deserialize server message")
    }

    async fn write_frame(&mut self, data: &[u8]) {
        self.stream
            .write_u32(data.len() as u32)
            .await
            .expect("failed to write frame length");
        self.stream
            .write_all(data)
            .await
            .expect("failed to write frame data");
        self.stream.flush().await.expect("failed to flush");
    }

    async fn read_frame(&mut self) -> Vec<u8> {
        let len = self
            .stream
            .read_u32()
            .await
            .expect("failed to read frame length") as usize;
        let mut buf = vec![0u8; len];
        self.stream
            .read_exact(&mut buf)
            .await
            .expect("failed to read frame data");
        buf
    }
}
