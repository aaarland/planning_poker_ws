use futures_util::{SinkExt, StreamExt, TryFutureExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;

static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

struct User {
    id: usize,
    name: String,
    room_name: String,
    vote: Option<usize>,
    is_owner: bool,
    tx: mpsc::UnboundedSender<Message>,
}

#[derive(Serialize, Deserialize, Clone)]
struct UserMessage {
    name: String,
    room_name: String,
    vote: Option<usize>,
    is_owner: bool,
    clear_all: bool,
}

type Users = Arc<RwLock<HashMap<usize, User>>>;
async fn user_connected(ws: WebSocket, users: Users) {
    let my_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    println!("new chat user: {}", my_id);
    let (mut user_ws_tx, mut user_ws_rx) = ws.split();
    let (tx, rx) = mpsc::unbounded_channel();
    let mut rx = UnboundedReceiverStream::new(rx);
    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            user_ws_tx
                .send(message)
                .unwrap_or_else(|e| {
                    eprintln!("websocket send error: {}", e);
                })
                .await;
        }
    });

    users.write().await.insert(
        my_id,
        User {
            id: my_id,
            name: "".to_string(),
            room_name: "".to_string(),
            vote: None,
            is_owner: false,
            tx,
        },
    );

    while let Some(result) = user_ws_rx.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error: {} {}", e, my_id);
                break;
            }
        };
        user_message(my_id, msg, &users).await;
    }
    user_disconnected(my_id, &users).await;
}

async fn user_message(my_id: usize, msg: Message, users: &Users) {
    // Skip any non-Text messages...
    let msg = if let Ok(s) = msg.to_str() {
        s
    } else {
        return;
    };

    let user_msg: UserMessage = match serde_json::from_str(msg) {
        Ok(msg) => msg,
        Err(e) => {
            eprintln!("error: {} {}", e, msg);
            return;
        }
    };
    match users.write().await.get_mut(&my_id) {
        Some(mut user) => {
            user.name = user_msg.name.clone();
            user.room_name = user_msg.room_name.clone();
            user.vote = user_msg.vote.clone();
            user.is_owner = user_msg.is_owner.clone();
        }
        None => {
            eprintln!("error: user not found {}", my_id);
            return;
        }
    };



    let clear_all = user_msg.clear_all.clone();

    if clear_all && user_msg.is_owner{
        for user in users.write().await.values_mut() {
            user.vote = None;
        }
    }

    let my_room = users.read().await.get(&my_id).unwrap().room_name.clone();
    let server_users: Vec<_> = users
        .read()
        .await
        .values()
        .map(|user| UserMessage {
            name: user.name.clone(),
            room_name: user.room_name.clone(),
            vote: user.vote.clone(),
            is_owner: user.is_owner.clone(),
            clear_all: user_msg.clear_all.clone(),
        })
        .collect();

    // New message from this user, send it to everyone else (except same uid)...
    for (&uid, user) in users.read().await.iter() {
        if my_id != uid && user.room_name == my_room {
            let new_server_msg = ServerMessage {
                room_name: my_room.clone(),
                users: server_users.clone(),
            };

            let serialized_message = serde_json::to_string(&new_server_msg).unwrap();

            if let Err(_disconnected) = user.tx.send(Message::text(serialized_message)) {
                // The tx is disconnected, our `user_disconnected` code
                // should be happening in another task, nothing more to
                // do here.
            }
        }
    }
}
#[derive(Serialize, Deserialize, Clone)]
struct ServerMessage {
    room_name: String,
    users: Vec<UserMessage>,
}

async fn user_disconnected(my_id: usize, users: &Users) {
    eprintln!("good bye user: {}", my_id);

    // Stream closed up, so remove from the user list
    users.write().await.remove(&my_id);
}
#[tokio::main]
async fn main() {
    let users = Users::default();
    let users = warp::any().map(move || users.clone());
    let chat = warp::path("chat")
        .and(warp::ws())
        .and(users)
        .map(|ws: warp::ws::Ws, users| ws.on_upgrade(move |socket| user_connected(socket, users)));

    warp::serve(chat).run(([127, 0, 0, 1], 3030)).await;
}
