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

use clap::Parser;
use libc;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::matrix_auth::MatrixSession;
use matrix_sdk::matrix_auth::MatrixSessionTokens;
use matrix_sdk::ruma::api::client::filter::FilterDefinition;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::OwnedDeviceId;
use matrix_sdk::ruma::OwnedRoomId;
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::Client;
use matrix_sdk::RoomState;
use matrix_sdk::SessionMeta;
use serde::Deserialize;
use serde::Serialize;
use serde_json;
use std::env;
use std::error::Error;
use std::io;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::path::PathBuf;
use termios;
use tokio::fs;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use url::Url;

// Struct for Session and homeserver.
// Store the homeserver explicitly because it might not be discoverable from the user ID.
#[derive(Serialize, Deserialize, Debug)]
struct Session {
    // Serialize is not implemented for Url
    homeserver: String,
    user_id: OwnedUserId,
    device_id: OwnedDeviceId,
    access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true)]
struct Args {
    /// The message subject
    #[arg(short)]
    subject: Option<String>,

    /// The recipient address
    #[arg(required = true, num_args = 1..)]
    addresses: Vec<OwnedRoomId>,
}

async fn load_session(file: &Path) -> Result<Session, Box<dyn Error>> {
    let mut f = File::open(file).await?;
    let mut buffer = Vec::new();
    f.read_to_end(&mut buffer).await?;
    let session = serde_json::from_slice(&buffer)?;
    Ok(session)
}

async fn save_session(file: &Path, session: &Session) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(file.parent().unwrap()).await?;
    let mut f = File::create(file).await?;
    let buffer = serde_json::to_vec(session)?;
    f.write_all(&buffer).await?;
    Ok(())
}

async fn send_message(
    client: &Client,
    room_id: &OwnedRoomId,
    message: &str,
) -> Result<(), Box<dyn Error>> {
    let room = match client.get_room(room_id).filter(|room| room.state() == RoomState::Joined) {
        Some(room) => room,
        None => client.join_room_by_id(room_id).await?,
    };
    let content = RoomMessageEventContent::text_plain(message);
    room.send(content).await?;
    Ok(())
}

fn prompt(message: &str) -> Result<String, io::Error> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    stdout.write_all(message.as_bytes())?;
    stdout.flush()?;
    let mut buffer = String::new();
    stdin.read_line(&mut buffer)?;
    Ok(String::from(buffer.strip_suffix("\n").unwrap_or(&buffer)))
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

fn gethostname() -> Result<String, io::Error> {
    let mut buffer: Vec<u8> = Vec::with_capacity(libc::_SC_HOST_NAME_MAX.try_into().unwrap());
    match unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut i8, buffer.capacity()) } {
        0 => Ok(String::from_utf8(buffer).unwrap()),
        _ => Err(io::Error::last_os_error()),
    }
}

async fn login(store_path: &Path) -> Result<Client, Box<dyn Error>> {
    let default_homeserver = String::from("matrix.org");
    let homeserver = prompt(&format!("Homeserver (default: {default_homeserver}): "))?;
    let homeserver = if !homeserver.is_empty() { homeserver } else { default_homeserver };
    let homeserver = if homeserver.starts_with("https://") || homeserver.starts_with("http://") { homeserver } else { format!("https://{homeserver}") };

    let user = prompt("User: ")?;

    let password = getpass("Password: ")?;

    let default_device_name = gethostname().unwrap_or(String::from(""));
    let device_name = prompt(&format!("Device name (default: {default_device_name}): "))?;
    let device_name = if !device_name.is_empty() { device_name } else { default_device_name };

    let default_display_name = format!("{user}@{device_name}", user=env::var("USER").unwrap());
    let display_name = prompt(&format!("Display name (default: {default_display_name}): "))?;
    let display_name = if !display_name.is_empty() { display_name } else { default_display_name };

    let client = Client::builder()
        .homeserver_url(Url::parse(&homeserver)?)
        .sqlite_store(&store_path, None)
        .build()
        .await?;
    let _response = client
        .matrix_auth()
        .login_username(&user, &password)
        .initial_device_display_name(&display_name)
        .device_id(&device_name)
        .await?;

    Ok(client)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    //#[cfg(debug_assertions)]
    //tracing_subscriber::fmt::init();

    unsafe { libc::umask(0o077) };
    let data_dir = env::var("XDG_DATA_HOME").and_then(|x| Ok(PathBuf::from(x))).or_else(|_| env::var("HOME").and_then(|x| Ok(PathBuf::from(x).join(".local/share")))).unwrap().join("matrixmail");
    let session_file = data_dir.join("login");

    let arg0 = env::args().nth(0).unwrap();
    let name = Path::new(&arg0).file_name().unwrap().to_str().unwrap();
    if name != "mail" && name != "mailx" {
        let client = login(&data_dir).await?;
        let auth_session = client.matrix_auth().session().unwrap();
        let session = Session {
            homeserver: client.homeserver().to_string(),
            user_id: auth_session.meta.user_id,
            device_id: auth_session.meta.device_id,
            access_token: auth_session.tokens.access_token,
            refresh_token: auth_session.tokens.refresh_token,
            sync_token: None,
        };
        save_session(&session_file, &session)
            .await
            .expect("Error saving session");
        return Ok(());
    }

    let args = Args::parse();
    let mut body = String::new();
    tokio::io::stdin().read_to_string(&mut body).await?;
    let message = match args.subject {
        Some(subject) => format!("{}\n\n{}", subject.trim(), body.trim()),
        None => String::from(body.trim()),
    };

    let mut session = load_session(&session_file).await.expect("Error loading session");
    let client = Client::builder()
        .homeserver_url(Url::parse(&session.homeserver)?)
        .sqlite_store(&data_dir, None)
        .build()
        .await?;
    let auth_session = MatrixSession {
        meta: SessionMeta {
            user_id: session.user_id.clone(),
            device_id: session.device_id.clone(),
        },
        tokens: MatrixSessionTokens {
            access_token: session.access_token.clone(),
            refresh_token: session.refresh_token.clone(),
        },
    };
    client.restore_session(auth_session).await.expect("Error restoring session");

    // Speed up initial sync for accounts in many rooms.
    let filter = FilterDefinition::with_lazy_loading();
    let mut sync_settings = SyncSettings::default().filter(filter.into());
    if let Some(sync_token) = session.sync_token {
        sync_settings = sync_settings.token(sync_token);
    }
    // Initial sync.
    loop {
        match client.sync_once(sync_settings.clone()).await {
            Ok(response) => {
                sync_settings = sync_settings.token(response.next_batch.clone());
                session.sync_token = Some(response.next_batch.clone());
                break;
            }
            Err(_) => {
                continue;
            }
        };
    }

    for address in &args.addresses {
        // Send message.
        send_message(&client, &address, &message).await.expect(&format!("Error sending message to {}", address));
        // Sync again.
        loop {
            match client.sync_once(sync_settings.clone()).await {
                Ok(response) => {
                    sync_settings = sync_settings.token(response.next_batch.clone());
                    session.sync_token = Some(response.next_batch.clone());
                    break;
                }
                Err(_) => {
                    continue;
                }
            };
        }
    }

    let auth_session = client.matrix_auth().session().unwrap();
    session.access_token = auth_session.tokens.access_token.clone();
    session.refresh_token = auth_session.tokens.refresh_token.clone();
    save_session(&session_file, &session).await.expect("Error saving session");

    Ok(())
}
