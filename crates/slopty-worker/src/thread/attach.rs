//! The files sent with a message, read on the worker for the adapter that sends them
//! (`slopty_agent::attach`).

use std::path::Path;

use slopty_agent::attach::{self, Attached};
use tokio::io::AsyncReadExt as _;

/// Whether `paths` can go with a message: few enough, and each a file or a folder here.
///
/// Why not, in words. A folder goes by its path, as a file too large to be a picture does. A
/// look at each one's metadata only, so it is quick enough to decide an intent by.
///
/// # Errors
///
/// When there are more than [`attach::MAX`], or one is not an absolute path to something here.
pub fn check(paths: &[String]) -> Result<(), String> {
    if paths.len() > attach::MAX {
        return Err(format!("A message takes at most {} files", attach::MAX));
    }
    for path in paths {
        if !Path::new(path).is_absolute() {
            return Err(format!("{path} is not a path on this machine"));
        }
        if std::fs::metadata(path).is_err() {
            return Err(format!("There is nothing at {path} here"));
        }
    }
    Ok(())
}

/// Each of `paths`, read: a picture whole, anything else by its path.
///
/// A picture is one as its first bytes say, no larger than [`attach::PICTURE_MAX`]. A file that
/// cannot be read goes by its path too, so the message is never lost for it, and the agent says
/// what it found there.
pub async fn read(paths: &[String]) -> Vec<Attached> {
    let mut read = Vec::with_capacity(paths.len());
    for path in paths {
        let attached = match picture(path).await {
            Ok(Some((media_type, bytes))) => {
                Attached::Picture { path: path.clone(), media_type, bytes }
            }
            Ok(None) => Attached::File { path: path.clone() },
            Err(e) => {
                tracing::warn!(path, "an attachment could not be read: {e}");
                Attached::File { path: path.clone() }
            }
        };
        read.push(attached);
    }
    read
}

/// The picture at `path` and its media type, or `None` when it is no picture (a folder is none)
/// or too large to send as one.
async fn picture(path: &str) -> std::io::Result<Option<(&'static str, Vec<u8>)>> {
    let mut file = tokio::fs::File::open(path).await?;
    let meta = file.metadata().await?;
    if !meta.is_file() || meta.len() > attach::PICTURE_MAX {
        return Ok(None);
    }
    let mut head = [0_u8; attach::HEAD];
    let mut got = 0;
    while got < head.len() {
        let Some(rest) = head.get_mut(got..) else { break };
        let n = file.read(rest).await?;
        if n == 0 {
            break;
        }
        got = got.saturating_add(n);
    }
    let Some(media_type) = head.get(..got).and_then(attach::picture_type) else {
        return Ok(None);
    };
    let mut bytes = head.get(..got).map(<[u8]>::to_vec).unwrap_or_default();
    file.read_to_end(&mut bytes).await?;
    Ok(Some((media_type, bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: [u8; 12] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];

    /// A picture is read whole by its bytes whatever it is named; text goes by its path, and so
    /// do a folder and a file that went before it was read.
    #[tokio::test]
    async fn pictures_are_read_whole_and_the_rest_go_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("shot.dat");
        std::fs::write(&shot, PNG).unwrap();
        let notes = dir.path().join("notes.png");
        std::fs::write(&notes, "# not a picture").unwrap();
        let gone = dir.path().join("gone.png");
        let folder = dir.path().join("src.png");
        std::fs::create_dir_all(&folder).unwrap();
        let paths: Vec<String> =
            [&shot, &notes, &gone, &folder].iter().map(|p| p.display().to_string()).collect();
        let read = read(&paths).await;
        assert_eq!(
            read,
            [
                Attached::Picture {
                    path: paths[0].clone(),
                    media_type: "image/png",
                    bytes: PNG.to_vec()
                },
                Attached::File { path: paths[1].clone() },
                Attached::File { path: paths[2].clone() },
                Attached::File { path: paths[3].clone() },
            ]
        );
    }

    /// Files and folders here go: a relative path or a missing file is refused in words, and so
    /// are more files than a message takes.
    #[test]
    fn files_and_folders_here_are_taken() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a").unwrap();
        let file = file.display().to_string();
        assert_eq!(check(std::slice::from_ref(&file)), Ok(()));
        assert_eq!(check(&[dir.path().display().to_string()]), Ok(()), "a folder, by its path");
        assert_eq!(check(&[]), Ok(()));
        for bad in ["a.txt".to_owned(), format!("{file}.gone")] {
            assert!(check(std::slice::from_ref(&bad)).is_err(), "{bad}");
        }
        assert!(check(&vec![file; attach::MAX + 1]).is_err(), "too many");
    }
}
