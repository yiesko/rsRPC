use std::{
  collections::HashMap,
  sync::{Arc, Mutex, mpsc},
};

use serde_json::Value;
use simple_websockets::{Event, EventHub, Message, Responder};

use crate::{
  cmd::{ActivityCmd, ActivityCmdArgs},
  commands, log,
  server::utils::CONNECTION_REPONSE,
  url_params::get_url_params,
};

// (last activity, client_id from the connect query, responder)
type ActivityResponder = (Option<ActivityCmd>, Option<String>, Responder);

#[derive(Clone)]
pub struct WebsocketConnector {
  server: Arc<Mutex<Option<EventHub>>>,
  pub clients: Arc<Mutex<HashMap<u64, ActivityResponder>>>,

  event_sender: mpsc::Sender<ActivityCmd>,
}

impl WebsocketConnector {
  pub fn new(
    event_sender: mpsc::Sender<ActivityCmd>,
    ws_port_start: u16,
    ws_port_end: u16,
  ) -> Self {
    // Try starting websocket server on the configured range, bound to
    // loopback only (games always connect to 127.0.0.1).
    for port in ws_port_start..=ws_port_end {
      let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(_) => {
          log!("[Websocket] Failed to start server on port {}", port);
          continue;
        }
      };

      match simple_websockets::launch_from_listener(listener) {
        Ok(server) => {
          log!("[Websocket] Server started on port {}", port);
          return Self {
            server: Arc::new(Mutex::new(Some(server))),
            clients: Arc::new(Mutex::new(HashMap::new())),
            event_sender,
          };
        }
        Err(_) => {
          log!("[Websocket] Failed to start server on port {}", port);
        }
      }
    }

    log!("[Websocket] Failed to start server on any port");
    std::process::exit(1);
  }

  pub fn start(&mut self, set_activity: bool, secondary_events: bool) {
    let server = self
      .server
      .lock()
      .unwrap()
      .take()
      .expect("Websocket server already started");
    let clients = self.clients.clone();
    let event_sender = self.event_sender.clone();

    std::thread::spawn(move || {
      let mut clients = clients.lock().unwrap();

      loop {
        log!("[Websocket] Polling for events...");

        match server.poll_event() {
          Event::Connect(client_id, responder) => {
            let connection = responder.connection_details();
            let url_params = get_url_params(connection.uri.clone());
            let version = url_params.get("v").unwrap_or(&"0".to_string()).clone();
            let encoding = url_params
              .get("encoding")
              .unwrap_or(&"json".to_string())
              .clone();
            let ws_client_id = url_params.get("client_id").cloned();

            log!("[Websocket] Client {} connected", client_id);

            if version != "1" || encoding != "json" {
              log!(
                "[Websocket] Invalid connection from client {} (v={}, encoding={}), closing",
                client_id,
                version,
                encoding
              );
              responder.close();
              continue;
            }

            responder.send(Message::Text(CONNECTION_REPONSE.to_string()));

            clients.insert(client_id, (None, ws_client_id, responder));
          }
          Event::Disconnect(client_id) => {
            log!("[Websocket] Client {} disconnected", client_id);
            let responder = match clients.remove(&client_id) {
              Some(responder) => responder,
              // A client that was never inserted (invalid handshake) was
              // closed directly, nothing to clean up.
              None => continue,
            };

            handle_disconnect(client_id, &event_sender, &responder);
          }
          Event::Message(client_id, message) => {
            log!(
              "[Websocket] Received message from client {}: {:?}",
              client_id,
              message
            );

            let responder = match clients.get_mut(&client_id) {
              Some(responder) => responder,
              None => continue,
            };
            let message = match message {
              Message::Text(text) => text,
              _ => "".to_string(),
            };

            // If not ActivityCmd, ignore
            let event: ActivityCmd = match serde_json::from_str(&message) {
              Ok(event) => event,
              Err(e) => {
                log!("[Websocket] Invalid message from client {}", client_id);
                log!("[Websocket] Error: {}", e);
                continue;
              }
            };

            // If origin isn't a Discord URL, ignore
            let origin = responder.2.connection_details().headers.get("origin");

            if let Some(origin) = origin {
              let value = origin.to_str().unwrap_or_default();
              let valid = [
                "https://discord.com",
                "https://canary.discord.com",
                "https://ptb.discord.com",
              ];

              if !valid.contains(&value) {
                log!("[Websocket] Invalid origin from client {}", client_id);
                continue;
              }
            }

            match event.cmd.as_str() {
              "INVITE_BROWSER" | "GUILD_TEMPLATE_BROWSER" => {
                if !secondary_events {
                  continue;
                }

                handle_browser_command(&event, &event_sender, &responder.2)
              }
              "DEEP_LINK" => handle_deep_link(&event, &responder.2),
              "CONNECTIONS_CALLBACK" => handle_connections_callback(&event, &responder.2),
              "SET_ACTIVITY" => {
                if !set_activity {
                  continue;
                }

                handle_set_activity(&event, &event_sender, responder)
              }
              _ => {
                log!("[Websocket] Unknown command: {}", event.cmd);
              }
            }
          }
        }
      }
    });
  }
}

fn event_args_as_hashmap(args: Option<ActivityCmdArgs>) -> HashMap<String, Value> {
  // Serde serialize the args
  let args = match args {
    Some(args) => serde_json::to_value(&args).unwrap_or(Value::Null),
    None => Value::Null,
  };

  // Re-deserialize the args as a hashmap, preserving value types
  match args {
    Value::Object(map) => map.into_iter().collect(),
    _ => HashMap::new(),
  }
}

fn handle_browser_command(
  event: &ActivityCmd,
  event_sender: &mpsc::Sender<ActivityCmd>,
  responder: &Responder,
) {
  // Let's just assume this went well I don't care
  let response = ActivityCmd {
    application_id: event.application_id.clone(),
    cmd: event.cmd.clone(),
    args: None,
    data: Some(event_args_as_hashmap(event.args.clone())),
    evt: None,
    nonce: event.nonce.clone(),
  };

  // Send the event away!
  if event_sender.send(event.clone()).is_err() {
    log!("[Websocket] Event receiver gone, dropping message");
    return;
  }

  // Respond
  responder.send(Message::Text(serde_json::to_string(&response).unwrap()));
}

fn handle_deep_link(event: &ActivityCmd, responder: &Responder) {
  let response = ActivityCmd {
    application_id: event.application_id.clone(),
    cmd: event.cmd.clone(),
    args: None,
    data: None,
    evt: None,
    nonce: event.nonce.clone(),
  };

  responder.send(Message::Text(serde_json::to_string(&response).unwrap()));
}

fn handle_connections_callback(event: &ActivityCmd, responder: &Responder) {
  let mut data = HashMap::new();
  data.insert("code".to_string(), Value::Number(1000.into()));

  let response = ActivityCmd {
    application_id: event.application_id.clone(),
    cmd: event.cmd.clone(),
    args: None,
    data: Some(data),
    evt: Some("ERROR".to_string()),
    nonce: event.nonce.clone(),
  };

  responder.send(Message::Text(serde_json::to_string(&response).unwrap()));
}

fn handle_set_activity(
  event: &ActivityCmd,
  event_sender: &mpsc::Sender<ActivityCmd>,
  responder: &mut ActivityResponder,
) {
  // Fall back to the client_id provided on connect (query param) when the
  // command itself does not carry an application_id.
  let mut event = event.clone();
  if event.application_id.is_none() {
    event.application_id = responder.1.clone();
  }

  // Apply field fixes so the confirmation reply carries labels/urls (fix is
  // idempotent, so the event_loop applying it again is harmless).
  event.fix();

  // Set the last activity for the client
  responder.0 = Some(event.clone());

  if event_sender.send(event.clone()).is_err() {
    log!("[Websocket] Event receiver gone, dropping message");
    return;
  }

  // Confirm to the game client (arRPC-shaped reply); some RPC libraries
  // wait for this before considering the presence set.
  if let Some(response) = commands::set_activity_response(&event) {
    responder.2.send(Message::Text(response));
  }
}

fn handle_disconnect(
  _client_id: u64,
  event_sender: &mpsc::Sender<ActivityCmd>,
  responder: &ActivityResponder,
) {
  if let Some(ref activity_cmd) = responder.0 {
    // Send empty activity. pid defaults to 0 when the last command had
    // no args (malformed SET_ACTIVITY): pid 0 is never a genuine clear,
    // so it is safely ignored downstream — and this never panics, which
    // would kill the whole bridge poll loop (stuck presence for everyone).
    let activity_cmd = ActivityCmd {
      application_id: activity_cmd.application_id.clone(),
      cmd: "SET_ACTIVITY".to_string(),
      data: None,
      evt: None,
      args: Some(ActivityCmdArgs {
        pid: Some(
          activity_cmd
            .args
            .as_ref()
            .and_then(|args| args.pid)
            .unwrap_or_default(),
        ),
        activity: None,
        code: None,
      }),
      nonce: activity_cmd.nonce.clone(),
    };

    if event_sender.send(activity_cmd).is_err() {
      log!("[Websocket] Event receiver gone, dropping clear");
    }
  }
}
