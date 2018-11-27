use super::schema::verfploeter::{Metadata, Task};
use super::schema::verfploeter_grpc::VerfploeterClient;

use futures::sync::oneshot;
use futures::*;
use grpcio::{ChannelBuilder, Environment};
//use std::sync::mpsc::{Receiver, Sender};
use clap::ArgMatches;
use futures::sync::mpsc::{Receiver, Sender};
use std::collections::HashMap;
use std::sync::Arc;

mod handlers;
use self::handlers::ping::{PingInbound, PingOutbound};
use self::handlers::{ChannelType, TaskHandler};
use std::time::Duration;

pub struct Client {
    grpc_client: Arc<VerfploeterClient>,
    task_handlers: HashMap<String, Box<dyn TaskHandler>>,
    metadata: Metadata,
}

impl Client {
    pub fn new(args: &ArgMatches) -> Client {
        // Setup GRPC client
        let host = args.value_of("server").unwrap();
        let env = Arc::new(Environment::new(1));
        let channel = ChannelBuilder::new(env)
            .keepalive_time(Duration::from_secs(180))
            .keepalive_timeout(Duration::from_secs(180))
            .max_send_message_len(100 * 1024 * 1024)
            .max_receive_message_len(100 * 1024 * 1024)
            .connect(host);
        let grpc_client = Arc::new(VerfploeterClient::new(channel));

        // Setup metadata
        let mut metadata = Metadata::new();
        metadata.set_hostname(args.value_of("hostname").unwrap().to_string());
        metadata.set_version(env!("CARGO_PKG_VERSION").to_string());

        // Setup task_handlers
        let mut task_handlers: HashMap<String, Box<dyn TaskHandler>> = HashMap::new();
        task_handlers.insert("ping_outbound".to_string(), Box::new(PingOutbound::new()));
        task_handlers.insert(
            "ping_inbound".to_string(),
            Box::new(PingInbound::new(metadata.clone(), grpc_client.clone())),
        );

        Client {
            grpc_client,
            task_handlers,
            metadata,
        }
    }

    pub fn start(mut self) {
        let res = self.grpc_client.connect(&self.metadata);
        if let Ok(stream) = res {
            // Get tx channel for ping_outbound
            let tx = match self
                .task_handlers
                .get_mut("ping_outbound")
                .unwrap()
                .get_channel()
            {
                ChannelType::Task { sender, .. } => sender.unwrap(),
                _ => panic!("ping_outbound has wrong tx channel type"),
            };

            // Signal finish
            let (finish_tx, finish_rx) = oneshot::channel();

            // For now we only have a ping task, in the future we can have a match here
            // that sends tasks to different threads for processing
            let f = stream
                .for_each({
                    let tx = tx.clone();
                    move |i| {
                        if i.has_ping() {
                            debug!("got ping task");
                            tx.clone().send(i).wait().unwrap();
                            debug!("sent to handler");
                        } else {
                            debug!("got keepalive");
                        }
                        futures::future::ok(())
                    }
                })
                .map_err(|e| {
                    debug!("{}", e);
                    finish_tx.send(()).unwrap();
                });

            self.grpc_client.spawn(f);

            // Start all task handlers
            for (i, v) in &mut self.task_handlers {
                v.start();
                debug!("started {} task handler", i);
            }

            // Wait for process to finish
            finish_rx.map(|_| ()).wait().unwrap();
            debug!("task stream closed");

            // Stop all task handlers
            for (i, v) in &mut self.task_handlers {
                debug!("signaling {} to exit", i);
                v.exit();
                debug!("exited {} task handler", i);
            }
        }
    }
}
