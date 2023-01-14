/*
matrixmail - POSIX mailx send mode over Matrix
Copyright (C) 2022  Axel Pirek

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program.  If not, see <https://www.gnu.org/licenses/>.
*/

use std::{
    env,
    error::Error,
    io::{self, Write},
    os::unix::io::AsRawFd,
    path::{Path, PathBuf},
    sync::Arc,
};
use clap::Parser;
use gethostname::gethostname;
use libc;
use matrix_sdk::{
    Client,
    room::{Joined, Room},
    ruma::{
        events::{
            SyncStateEvent,
            room::{
                member::{MembershipState, RoomMemberEventContent},
                message::{RoomMessageEventContent},
            },
        },
        OwnedDeviceId,
        OwnedRoomId,
        OwnedUserId,
    },
    config::SyncSettings,
    reqwest::Url,
    Session,
    store::make_store_config,
};
use serde::{Deserialize, Serialize};
use serde_json;
use termios;
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};

// Struct for Session and homeserver.
// Store the homeserver explicitly because it might not be discoverable from the user ID.
#[derive(Serialize, Deserialize, Debug)]
struct Login {
    access_token: String,
    device_id: OwnedDeviceId,
    // Serialize is not implemented for Url
    homeserver: String,
    user_id: OwnedUserId,
}


#[derive(clap::Parser, Debug)]
#[clap(disable_help_flag = true)]
struct Args {
    /// The message subject
    #[clap(short)]
    subject: Option<String>,

    /// The recipient address
    #[clap(required = true, min_values = 1)]
    addresses: Vec<OwnedRoomId>,
}

async fn load_login(file: &Path) -> Result<Login, Box<dyn Error>> {
    let mut f = File::open(file).await?;
    let mut buffer = Vec::new();
    f.read_to_end(&mut buffer).await?;
    let login = serde_json::from_slice(&buffer)?;
    Ok(login)
}

async fn save_login(file: &Path, login: &Login) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(file.parent().unwrap()).await?;
    let mut f = File::create(file).await?;
    let buffer = serde_json::to_vec(login)?;
    f.write_all(&buffer).await?;
    Ok(())
}

async fn join_room(client: &Client, room_id: &OwnedRoomId) -> Result<Joined, Box<dyn Error>> {
    let joined = Arc::new(Notify::new());
    let event_handler = client.add_event_handler({
        let room_id = room_id.clone();
        let joined = joined.clone();
        move |event: SyncStateEvent<RoomMemberEventContent>, room: Room, client: Client| {
            let room_id = room_id.clone();
            let joined = joined.clone();
            async move {
                if room.room_id() == room_id && *event.state_key() == client.user_id().unwrap().to_string() && *event.membership() == MembershipState::Join {
                    joined.notify_one();
                }
            }
        }
    });
    client.join_room_by_id(room_id).await?;
    joined.notified().await;
    client.remove_event_handler(event_handler);
    Ok(client.get_joined_room(room_id).unwrap())
}

async fn send_message(client: &Client, room_id: &OwnedRoomId, message: &str) -> Result<(), Box<dyn Error>> {
    let room = match client.get_joined_room(room_id) {
        Some(room) => room,
        None => join_room(client, room_id).await?,
    };
    let content = RoomMessageEventContent::text_plain(message);
    room.send(content, None).await.unwrap();
    Ok(())
}

fn prompt(message: &str) -> Result<String, io::Error> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    stdout.write_all(message.as_bytes())?;
    stdout.flush()?;
    let mut buffer = String::new();
    stdin.read_line(&mut buffer)?;
    Ok(String::from(buffer.strip_suffix("\n").or(Some(&buffer)).unwrap()))
}

fn getpass(message: &str) -> Result<String, io::Error> {
    let stdin = io::stdin().as_raw_fd();
    let old_termios = termios::Termios::from_fd(stdin)?;
    let mut new_termios = old_termios;
    new_termios.c_lflag &= !termios::ECHO;
    termios::tcsetattr(stdin, termios::TCSAFLUSH, &new_termios)?;
    let pass = prompt(message);
    termios::tcsetattr(stdin, termios::TCSAFLUSH, &old_termios)?;
    io::stdout().write_all(b"\n")?;
    pass
}

async fn setup(login_file: &Path) -> Result<(), Box<dyn Error>> {
    let default_homeserver = String::from("matrix.org");
    let homeserver = prompt(&format!("Homeserver (default: {default_homeserver}): "))?;
    let homeserver = if !homeserver.is_empty() { homeserver } else { default_homeserver };
    let homeserver = if homeserver.starts_with("https://") || homeserver.starts_with("http://") { homeserver } else { format!("https://{homeserver}") };

    let user = prompt("User: ")?;

    let password = getpass("Password: ")?;

    let default_device_name = gethostname().into_string().unwrap();
    let device_name = prompt(&format!("Device name (default: {default_device_name}): "))?;
    let device_name = if !device_name.is_empty() { device_name } else { default_device_name };

    let default_display_name = format!("{user}@{device_name}", user=env::var("USER").unwrap());
    let display_name = prompt(&format!("Display name (default: {default_display_name}): "))?;
    let display_name = if !display_name.is_empty() { display_name } else { default_display_name };

    let client = Client::new(Url::parse(&homeserver)?).await?;
    let response = client
        .login_username(&user, &password)
        .initial_device_display_name(&display_name)
        .device_id(&device_name)
        .send()
        .await?;

    let login = Login {
        access_token: response.access_token,
        device_id: response.device_id,
        homeserver: client.homeserver().await.to_string(),
        user_id: response.user_id,
    };
    save_login(&login_file, &login).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    unsafe { libc::umask(0o077) };
    let data_dir = env::var("XDG_DATA_HOME").and_then(|x| Ok(PathBuf::from(x))).or_else(|_| env::var("HOME").and_then(|x| Ok(PathBuf::from(x).join(".local/share")))).unwrap().join("matrixmail");
    let login_file = data_dir.join("login");

    let arg0 = env::args().nth(0).unwrap();
    let name = Path::new(&arg0).file_name().unwrap().to_str().unwrap();
    if name != "mail" && name != "mailx" {
        return setup(&login_file).await;
    }

    let args = Args::parse();

    let login = load_login(&login_file).await?;
    let client = Client::builder()
        .homeserver_url(Url::parse(&login.homeserver)?)
        .store_config(make_store_config(data_dir, None)?)
        .build().await?;
    // FIXME Deserialization errors in Store.restore_ression after matrix-sdk update.
    // Workaround: rm -r ~/.local/share/matrixmail/matrix-sdk-state
    client.restore_login(Session { access_token: login.access_token, refresh_token: None, user_id: login.user_id, device_id: login.device_id }).await.expect("Error restoring session:");

    client.sync_once(SyncSettings::default()).await?;

    let mut body = String::new();
    tokio::io::stdin().read_to_string(&mut body).await?;
    let message = match args.subject {
        Some(subject) => format!("{}\n\n{}", subject.trim(), body.trim()),
        None => String::from(body.trim()),
    };

    for address in &args.addresses {
        send_message(&client, &address, &message).await?;
        client.sync_once(SyncSettings::default().token(client.sync_token().await.unwrap())).await?;
    }

    Ok(())
}
