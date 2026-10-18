//! Linux: a process's mappings, from `/proc/<pid>/maps`.
//!
//! Linux 6.11 answers `PROCMAP_QUERY` on the open maps file, one binary record
//! per mapping, which spares formatting and parsing every line. Older kernels
//! answer `ENOTTY`, and then the same file is read as text.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use rustix::io::Errno;
use rustix::ioctl::{Updater, ioctl, opcode};

use crate::process::Region;

/// `struct procmap_query` from `<linux/fs.h>`.
#[repr(C)]
#[derive(Default)]
struct ProcmapQuery {
    size: u64,
    query_flags: u64,
    query_addr: u64,
    vma_start: u64,
    vma_end: u64,
    vma_flags: u64,
    vma_page_size: u64,
    vma_offset: u64,
    inode: u64,
    dev_major: u32,
    dev_minor: u32,
    vma_name_size: u32,
    build_id_size: u32,
    vma_name_addr: u64,
    build_id_addr: u64,
}

const PROCMAP_QUERY: rustix::ioctl::Opcode = opcode::read_write::<ProcmapQuery>(b'f', 17);

const VMA_READABLE: u64 = 0x01;
const VMA_WRITABLE: u64 = 0x02;
const VMA_EXECUTABLE: u64 = 0x04;
const VMA_SHARED: u64 = 0x08;
const COVERING_OR_NEXT_VMA: u64 = 0x10;

/// A mapping with what backs it.
struct Mapping {
    region: Region,
    /// The device and inode `maps` names; the inode is 0 for anonymous memory.
    dev: u64,
    inode: u64,
    /// The path `maps` names, when asked for.
    path: Option<PathBuf>,
}

/// The file behind a mapping, as `maps` names it. That device is the
/// superblock's, which on a filesystem with subvolumes (btrfs) is not the one
/// `stat` reports, so the path is given to check it against.
pub struct Backing {
    pub dev: u64,
    pub inode: u64,
    pub path: Option<PathBuf>,
}

/// Every mapping `maps` lists, in address order.
pub fn regions(maps: &str) -> io::Result<Vec<Region>> {
    Ok(mappings(maps, false)?
        .into_iter()
        .map(|mapping| mapping.region)
        .collect())
}

/// The file behind every mapping `maps` lists that has one.
pub fn backing_files(maps: &str) -> io::Result<Vec<Backing>> {
    Ok(mappings(maps, true)?
        .into_iter()
        .filter(|mapping| mapping.inode != 0)
        .map(|mapping| Backing {
            dev: mapping.dev,
            inode: mapping.inode,
            path: mapping.path,
        })
        .collect())
}

fn mappings(maps: &str, names: bool) -> io::Result<Vec<Mapping>> {
    let mut file = File::open(maps)?;
    match query(&file, names) {
        Ok(mappings) => Ok(mappings),
        // The ioctl leaves the file offset alone, so the text is still unread.
        Err(_) => {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            Ok(text.lines().filter_map(parse_mapping).collect())
        }
    }
}

/// Walks the mappings by asking for the first one at or after the end of the
/// last. Unlike the text, this leaves out the gate area (`[vsyscall]`).
fn query(file: &File, names: bool) -> Result<Vec<Mapping>, Errno> {
    let mut mappings = Vec::new();
    let mut name = vec![0u8; if names { 4096 } else { 0 }];
    let mut addr = 0;
    loop {
        let mut q = ProcmapQuery {
            size: size_of::<ProcmapQuery>() as u64,
            query_flags: COVERING_OR_NEXT_VMA,
            query_addr: addr,
            vma_name_size: name.len() as u32,
            vma_name_addr: if names { name.as_mut_ptr() as u64 } else { 0 },
            ..Default::default()
        };
        // SAFETY: `q` is a `procmap_query` sized as such, and its name buffer
        // is either `name`, of the length given, or absent.
        match unsafe { ioctl(file, Updater::<PROCMAP_QUERY, _>::new(&mut q)) } {
            Ok(()) => {}
            // Nothing at or past `addr`: the walk is done.
            Err(Errno::NOENT) => return Ok(mappings),
            // The kernel has set `vma_name_size` to what the name needs.
            Err(Errno::NAMETOOLONG) if names => {
                name.resize(q.vma_name_size as usize, 0);
                continue;
            }
            Err(errno) => return Err(errno),
        }
        let path = (names && q.inode != 0 && q.vma_name_size > 1)
            .then(|| &name[..q.vma_name_size as usize - 1])
            .map(|bytes| PathBuf::from(OsStr::from_bytes(bytes)));
        mappings.push(Mapping {
            region: Region {
                start: q.vma_start,
                end: q.vma_end,
                read: q.vma_flags & VMA_READABLE != 0,
                write: q.vma_flags & VMA_WRITABLE != 0,
                execute: q.vma_flags & VMA_EXECUTABLE != 0,
                shared: q.vma_flags & VMA_SHARED != 0,
                offset: q.vma_offset,
            },
            dev: libc::makedev(q.dev_major, q.dev_minor),
            inode: q.inode,
            path,
        });
        addr = q.vma_end;
    }
}

/// `lo-hi perms offset major:minor inode path`, the numbers of the device in
/// hex and the path, if any, after a run of spaces.
fn parse_mapping(line: &str) -> Option<Mapping> {
    let mut fields = line.split_whitespace();
    let (low, high) = fields.next()?.split_once('-')?;
    let perms = fields.next()?.as_bytes();
    let offset = u64::from_str_radix(fields.next()?, 16).ok()?;
    let (major, minor) = fields.next()?.split_once(':')?;
    let inode = fields.next()?.parse::<u64>().ok()?;
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start();
        rest = &rest[rest.find(char::is_whitespace).unwrap_or(rest.len())..];
    }
    let path = rest.trim_start();
    Some(Mapping {
        region: Region {
            start: u64::from_str_radix(low, 16).ok()?,
            end: u64::from_str_radix(high, 16).ok()?,
            read: perms.first() == Some(&b'r'),
            write: perms.get(1) == Some(&b'w'),
            execute: perms.get(2) == Some(&b'x'),
            shared: perms.get(3) == Some(&b's'),
            offset,
        },
        dev: libc::makedev(
            u32::from_str_radix(major, 16).ok()?,
            u32::from_str_radix(minor, 16).ok()?,
        ),
        inode,
        path: (inode != 0 && !path.is_empty()).then(|| PathBuf::from(path)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_line(line: &str) -> Option<Region> {
        parse_mapping(line).map(|mapping| mapping.region)
    }

    #[test]
    fn a_maps_line_reads_as_its_region() {
        let region = parse_line("7f00-7f80 r-xs 00001000 08:01 1234   /usr/lib/libc.so.6").unwrap();
        assert_eq!(
            (region.start, region.end, region.offset),
            (0x7f00, 0x7f80, 0x1000)
        );
        assert!(region.read && !region.write && region.execute && region.shared);
    }

    #[test]
    fn the_query_and_the_text_agree_on_this_functions_region() {
        let file = File::open("/proc/self/maps").unwrap();
        let Ok(queried) = query(&file, false) else {
            eprintln!("skipping: this kernel has no PROCMAP_QUERY");
            return;
        };
        let text = std::fs::read_to_string("/proc/self/maps").unwrap();
        let parsed: Vec<_> = text.lines().filter_map(parse_line).collect();
        let here = the_query_and_the_text_agree_on_this_functions_region as *const () as u64;
        let find = |regions: &[Region]| {
            *regions
                .iter()
                .find(|region| (region.start..region.end).contains(&here))
                .unwrap()
        };
        let queried: Vec<_> = queried.into_iter().map(|m| m.region).collect();
        assert_eq!(find(&queried), find(&parsed));
    }

    #[test]
    fn a_maps_line_names_its_device_inode_and_path() {
        let line = "7f00-7f80 r-xp 00001000 08:11 1234     /opt/with space/libc.so";
        let mapping = parse_mapping(line).unwrap();
        assert_eq!(mapping.dev, libc::makedev(0x8, 0x11));
        assert_eq!(mapping.inode, 1234);
        assert_eq!(mapping.path, Some(PathBuf::from("/opt/with space/libc.so")));
    }

    #[test]
    fn an_anonymous_maps_line_has_no_path() {
        let mapping = parse_mapping("55d0-55e0 rw-p 00000000 00:00 0 [heap]").unwrap();
        assert_eq!((mapping.inode, mapping.path), (0, None));
    }

    #[test]
    fn the_query_names_this_executable() {
        let file = File::open("/proc/self/maps").unwrap();
        let Ok(queried) = query(&file, true) else {
            eprintln!("skipping: this kernel has no PROCMAP_QUERY");
            return;
        };
        let exe = std::env::current_exe().unwrap();
        assert!(queried.iter().any(|m| m.path.as_deref() == Some(&exe)));
    }
}
