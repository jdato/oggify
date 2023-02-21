use log::*; 
use std::io::{self, BufRead, Read, Result};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{Local, Datelike, Timelike};
use http::Request;
use hyper::body::HttpBody;
use id3::frame::{PictureType, Comment};
use id3::{frame, TagLike, Version, Frame, Content, Timestamp};
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


pub fn read(args: Vec<String>) {
    assert!(
        args.len() == 3 || args.len() == 4,
        "Invalid number of arguments: [{}] please provide '3' or '4' arguments. Usage: {} user password [update_id3] < tracks_file",
        args.len(),
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
        if str.starts_with("#") {
            None
        } else {
            playlist_url
            .captures(&str)
            .or_else(|| {
                warn!("Cannot parse playlist from string {}", str);
                None
            })
            .and_then(|capture| SpotifyId::from_base62(&capture[1]).ok())
        }
      })
    })
    .for_each(|id| {
      info!("Getting playlist {:?}...", id.to_base62());
      let playlist = core
        .block_on(Playlist::get(&session, &id))
        .expect("Cannot get track metadata");

      playlist.contents.items.iter().for_each(|pl_item| {
        //
        info!("Reading track {:?}, form playlist {:?}", pl_item.id.to_base62(), playlist.name());
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

        // let track_copy = track.clone();

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

        let track_name = track.name;
        let fname_ogg = format!("{} - {}.ogg", artists_strs.join(", "), track_name).replace("/", "-");
        let fname_mp3 = format!("{} - {}.mp3", artists_strs.join(", "), track_name).replace("/", "-");

        let base_dir_name = "music";
        let playlist_dir_name = playlist.name();
        let target_dir_path = format!("{base_dir_name}/{playlist_dir_name}");
        let final_ogg_file_path = format!("{target_dir_path}/{fname_ogg}");
        let final_mp3_file_path = format!("{target_dir_path}/{fname_mp3}");

        

        match (std::path::Path::try_exists(Path::new(final_mp3_file_path.as_str())), args.get(3)) {
            (Ok(true), None) => debug!("Skipping {track_name} by {artists_strs:?}, because it already exists at '{final_mp3_file_path}'."),
            (Ok(exists), update_id3) => {
                let downloaded;
              
                if !exists {
                let files = track.files.iter();
                info!("Dowloading {track_name} by {artists_strs:?}.");
                if let Some((file_id, bitrate)) = track
                .files
                .get(&AudioFileFormat::OGG_VORBIS_320)
                .map(|f| (f, 320))
                .or(track.files.get(&AudioFileFormat::OGG_VORBIS_160).map(|f| (f, 160))) {
                    if bitrate != 320 {
                        info!("Bitrate: 320 not available!");
                        if bitrate == 160 {
                            warn!("Bitrate: 160!");
                        } else {
                            warn!("Bitrate: 160 also not available! These are here:\n{:#?}\n~", files.map(|f| f.0));
                        }
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
                    downloaded = true;
                  } else {
                    error!("Couldn't download {track_name} by {artists_strs:?}.\nThese files are here:\n{:#?}\nAction Required: Try to find the original EP / Single of the trrack above and add it to the playlist again.", //This is the track:\n{:#?}
                        files.map(|f| f.0),
                        // track_copy
                    );
                    downloaded = false;
                }
                } else {
                    downloaded = true;
                    info!("Skipping {track_name} by {artists_strs:?}, because it already exists at '{final_mp3_file_path}'.");
                  }
                    // TODO find more metadata - # of plays
                  if downloaded && (!exists || update_id3.map(|update| update.eq("update_id3")).unwrap_or(false)) {
                    info!("Updating Metadata for '{final_mp3_file_path}'.");
                    let mut tag = id3::Tag::new();

                    let album = core
                    .block_on(Album::get(&session, &track.album.id))
                    .expect("Cannot get album metadata");

                    tag.set_album(&album.name);
                    tag.set_title(track_name);
                  
                    let date = Local::now();
                    let time = date.time();

                    tag.set_date_released(Timestamp {
                      year: date.year(),
                      month: Some(date.month() as u8),
                      day: Some(date.day() as u8),
                      hour: Some(time.hour() as u8),
                      minute: Some(time.minute() as u8),
                      second: Some(time.second() as u8), 
                    });
                    tag.set_genre(playlist_dir_name.to_owned());
                    let genres = album.genres.join(", ");
                    let info = Frame::with_content("COMM", Content::Comment(Comment {
                      lang: "eng".to_owned(),
                      description: "".to_owned(),
                      text: format!("Playlist: {}{}", &playlist_dir_name, if !genres.is_empty() {format!(", Genres: {}", genres)} else {"".into()}),
                    }));
                    tag.add_frame(info);
                    tag.set_artist(artists_strs.join(", "));
                    let cover = core
                    .block_on(
                        async move {
                        if let Some(cover) = album.covers.first() {
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
                    if let Err(e) = tag.write_to_path(&final_mp3_file_path, Version::Id3v24) {
                        error!("Error ID3: {:?}, Path: {}", e, final_mp3_file_path);
                    }
                  }
                
            },
            (Err(e), _) => error!("An error occurred when trying to check for existing file: {final_mp3_file_path}. Error: {:?}", e),
        }
      });
    });
}