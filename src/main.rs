extern crate env_logger;
extern crate librespot_audio;
extern crate librespot_core;
extern crate librespot_metadata;
#[macro_use]
extern crate log;
extern crate regex;
extern crate scoped_threadpool;
extern crate tokio;

use std::env;
use std::io::{self, BufRead, Read, Result};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use env_logger::{Builder, Env};
use http::Request;
use hyper::body::HttpBody;
use id3::frame::{PictureType, Comment};
use id3::{frame, TagLike, Version};
use librespot_audio::{AudioDecrypt, AudioFile};
use librespot_core::authentication::Credentials;
use librespot_core::config::SessionConfig;
use librespot_core::http_client::HttpClient;
use librespot_core::session::Session;
use librespot_core::spotify_id::SpotifyId;
use librespot_metadata::Playlist;
use librespot_metadata::{audio::AudioFileFormat, Album, Artist, Metadata, Track};
use regex::Regex;
use scoped_threadpool::Pool;

fn main() {
    Builder::from_env(Env::default().default_filter_or("info")).init();

    let args: Vec<_> = env::args().collect();
    assert!(
        args.len() == 3 || args.len() == 4,
        "Usage: {} user password [helper_script] < tracks_file",
        args[0]
    );

    let core = tokio::runtime::Runtime::new().unwrap();
    let session_config = SessionConfig::default();

    let credentials = Credentials::with_password(args[1].to_owned(), args[2].to_owned());
    info!("Connecting ...");
    let session = core.block_on(async { Session::new(session_config, None) });

    core.block_on(Session::connect(&session, credentials, false))
        .unwrap();
    info!("Connected!");

    let mut threadpool = Pool::new(1);

    let playlist_url = Regex::new(r"open\.spotify\.com/playlist/([[:alnum:]]+)").unwrap();

    // Since I am passing the file via cargo ... < file, I can read it from stdin
    io::stdin()
    .lock()
    .lines()
    .filter_map(|line| {
      line.ok().and_then(|str| {
        playlist_url
          .captures(&str)
          .or_else(|| {
            warn!("Cannot parse playlist from string {}", str);
            None
          })
          .and_then(|capture| SpotifyId::from_base62(&capture[1]).ok())
      })
    })
    .for_each(|id| {
      info!("Getting playlist {:?}...", id.to_base62());
      let playlist = core
        .block_on(Playlist::get(&session, &id))
        .expect("Cannot get track metadata");

      playlist.contents.items.iter().for_each(|pl_item| {
        //
        info!("Getting track {:?}, form playlist {:?}", pl_item.id.to_base62(), playlist.name());
        let mut track = core
          .block_on(Track::get(&session, &pl_item.id))
          .expect("Cannot get track metadata");
        if !track.availability.is_empty() {
          warn!(
            "Track {:?} is not available, finding alternative...",
            id.to_base62()
          );
          let alt_track = track.alternatives.iter().find_map(|id| {
            let alt_track = core
              .block_on(Track::get(&session, id))
              .expect("Cannot get track metadata");
            match !alt_track.availability.is_empty() {
              true => Some(alt_track),
              false => None,
            }
          });
          track = alt_track.expect(&format!(
            "Could not find alternative for track {:?}",
            id.to_base62()
          ));
          warn!(
            "Found track alternative {:?} -> {:?}",
            id.to_base62(),
            track.id.to_base62()
          );
        }

        let artists_strs: Vec<_> = track
          .artists
          .iter()
          .map(|id| {
            core
              .block_on(Artist::get(&session, &id.id))
              .expect("Cannot get artist metadata")
              .name
          })
          .collect();
        debug!(
          "File formats: {}",
          track
            .files
            .keys()
            .map(|filetype| format!("{:?}", filetype))
            .collect::<Vec<_>>()
            .join(" ")
        );

        let track_name = track.name;
        let fname_ogg = format!("{} - {}.ogg", artists_strs.join(", "), track_name);
        let fname_mp3 = format!("{} - {}.mp3", artists_strs.join(", "), track_name);

        let base_dir_name = "music";
        let playlist_dir_name = playlist.name();
        let target_dir_path = format!("{base_dir_name}/{playlist_dir_name}");
        let final_ogg_file_path = format!("{target_dir_path}/{fname_ogg}");
        let final_mp3_file_path = format!("{target_dir_path}/{fname_mp3}");

        // Check if file exists
        
        match std::path::Path::try_exists(Path::new(final_mp3_file_path.as_str())) {
            Ok(true) => log::info!("Skipping {track_name} by {artists_strs:?}, because it already exists at '{final_mp3_file_path}'."),
            Ok(false) => {
                log::info!("Dowloading {track_name} by {artists_strs:?}.");
                if let Some((file_id, bitrate)) = track
                .files
                .get(&AudioFileFormat::OGG_VORBIS_320)
                .map(|f| (f, 320))
                .or(track.files.get(&AudioFileFormat::OGG_VORBIS_160).map(|f| (f, 160))) {
                    if bitrate == 160 {
                        log::warn!("Bitrate: 160!");
                    }
                    // .or(track.files.get(&AudioFileFormat::OGG_VORBIS_96))
                    //   .expect("Could not find the 320kbs OGG_VORBIS format for the track.");
                    let key = core
                    .block_on(session.audio_key().request(track.id, *file_id))
                    .ok();
                    let mut encrypted_file = core
                    .block_on(AudioFile::open(&session, *file_id, 320))
                    .unwrap();
                    let mut buffer = Vec::new();
                    let mut read_all: Result<usize> = Ok(0);
                    let fetched = AtomicBool::new(false);
                    threadpool.scoped(|scope| {
                    scope.execute(|| {
                        read_all = encrypted_file.read_to_end(&mut buffer);
                        fetched.store(true, Ordering::Release);
                    });
                    while !fetched.load(Ordering::Acquire) {
                        core.block_on(async { tokio::time::sleep(Duration::from_millis(100)).await });
                        // core.turn(Some(Duration::from_millis(100)));
                    }
                    });
                    read_all.expect("Cannot read file stream");
                    let mut decrypted_buffer = Vec::new();
                    AudioDecrypt::new(key, &buffer[..])
                    .read_to_end(&mut decrypted_buffer)
                    .expect("Cannot decrypt stream");

                    let album = core
                    .block_on(Album::get(&session, &track.album.id))
                    .expect("Cannot get album metadata");

                    let album_clone = album.clone();

                    match std::fs::create_dir(&target_dir_path) {
                        Ok(_) => info!("Created directory '{}'.", target_dir_path),
                        Err(e) => info!("Could not create directory '{}'. Message: {:?}", target_dir_path, e),
                    }
                    std::fs::write(&final_ogg_file_path, &decrypted_buffer[0xa7..]).expect("Cannot write decrypted track");
                    info!("Wrote file with filename: {}", fname_ogg);

                    // convert to mp3 highest quality
                    let output = Command::new("ffmpeg")
                    .arg("-i")
                    .arg(&format!("music/{}/{}", playlist_dir_name, fname_ogg.clone()))
                    .arg("-map_metadata")
                    .arg("0:s:0")
                    .arg("-id3v2_version")
                    .arg("3")
                    .arg("-codec:a")
                    .arg("libmp3lame")
                    .arg("-qscale:a")
                    .arg("1")
                    .arg(&final_mp3_file_path) // TO CHANGE
                    .output()
                    .expect("Failed to do convert");

                    info!("status: {}", output.status);

                    info!("Converted file with filename: {} to {}", fname_ogg.clone(), fname_mp3);
                    info!("Removing old .ogg file with filename: {}", fname_mp3);
                    if let Err(e) = std::fs::remove_file(&final_ogg_file_path) {
                    error!("Couldn't remove file: {:?}, error: {:?}", final_ogg_file_path, e)
                    }

                    // TODO find more metadata - # of plays

                    let mut tag = id3::Tag::new();
                    tag.set_genre(album.genres.join(", "));
                    tag.set_album(album.name);
                    tag.set_title(track_name);
                    tag.add_frame(Comment {
                        lang: "en".to_owned(),
                        description: "Bitrate".to_owned(),
                        text: bitrate.to_string(),
                    });
                    tag.set_artist(artists_strs.join(", "));
                    tag.add_frame(frame::Comment{ lang: "en".to_owned(), description: "A comment".to_owned(), text: "Plays xyz".to_owned() });
                    let cover = core
                    .block_on(
                        async move {
                        if let Some(cover) = album_clone.covers.first() {
                            if let Ok(req) = Request::builder()
                            .method("GET")
                            .uri(format!("https://i.scdn.co/image/{}", cover.id))
                            .body(hyper::body::Body::default()) {
                                if let Ok(cover_image) = HttpClient::new(None).request(req).await {
                                let mut data = Vec::new();
                                let mut body = cover_image.into_body();

                                while !body.is_end_stream() {
                                    if let Some(chunk) = body
                                    .data()
                                    .await
                                    .and_then(|body|
                                        body
                                        .ok()
                                        .and_then(|bytes|
                                        Some(bytes.to_vec())
                                        )
                                    ) {
                                        data.extend(chunk)
                                    }
                                }
                                return Some(data);
                                } else { return None; }
                            } else { return None; }
                            } else { return None; }
                        }
                    );
                    if let Some(jpeg) = cover {
                        tag.add_frame(frame::Picture {
                            mime_type: "image/jpeg".to_string(),
                            picture_type: PictureType::CoverFront,
                            description: "Cover Image".to_string(),
                            data: jpeg,
                        });
                    }
                    if let Err(e) = tag.write_to_path(&format!("music/{}/{}", playlist_dir_name, fname_mp3), Version::Id3v24) {
                        error!("Error ID3: {:?}", e);
                    }
                }
            },
            Err(e) => log::error!("An error occurred when trying to check for existing file: {final_mp3_file_path}. Error: {:?}", e),
        }
      });
    });
}
