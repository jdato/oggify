# oggify
Download entire Spotify playlists to Ogg Vorbis and convert them to mp3 (with a premium account).

This library uses [librespot](https://github.com/librespot-org/librespot). Props to @uways for the first implementation, I just extended it a little.

# Prerequisites
- ffmpeg must be installed on machine.
# Usage
To download your playlists simply use `cargo run "spot-username" "spot-pwd" < playlists_file`.
There must be a playlists file containing lines of 
```
oggify "spotify-premium-user" "spotify-premium-password" < tracks_list
```
Oggify reads from stdin and looks for a track URL or URI in each line. The two formats are those you get with the track menu items "Share->Copy Song Link" or "Share->Copy Song URI" in the Spotify client, for example `open.spotify.com/track/1xPQDRSXDN5QJWm7qHg5Ku` or `spotify:track:1xPQDRSXDN5QJWm7qHg5Ku`.

