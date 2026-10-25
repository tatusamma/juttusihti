//! Map tiles following the OpenStreetMap tile usage policy
//! (<https://operations.osmfoundation.org/policies/tiles/>):
//!
//! - only tiles in the current viewport are requested; there is no
//!   prefetching, bulk download or "download for offline",
//! - requests carry a distinct, stable User-Agent naming the app,
//! - `no-cache` is never sent; server `Cache-Control`/`Expires`/`ETag` are
//!   honoured, tiles without caching headers are kept for at least 7 days,
//!   and expired tiles are revalidated with conditional requests,
//! - the tile URL is configurable rather than hard-coded.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, EXPIRES, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

pub const OSM_TILE_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
pub const OSM_COPYRIGHT_URL: &str = "https://www.openstreetmap.org/copyright";
pub const OSM_FIX_THE_MAP_URL: &str = "https://www.openstreetmap.org/fixthemap";
pub const ATTRIBUTION: &str = "© OpenStreetMap contributors";

pub const TILE_SIZE: f32 = 256.0;
pub const MAX_ZOOM: u8 = 19;
/// Web Mercator latitude limit.
pub const MAX_LAT: f64 = 85.051_128_78;
/// Minimum retention when the server sends no caching headers.
pub const DEFAULT_FRESHNESS: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_TILE_BYTES: usize = 1024 * 1024;
const MAX_PARALLEL: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// Longitude/latitude → world coordinates in [0, 1] (Web Mercator).
pub fn lonlat_to_world(lon: f64, lat: f64) -> (f64, f64) {
    let lat = lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    let x = (lon + 180.0) / 360.0;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0;
    (x.rem_euclid(1.0), y.clamp(0.0, 1.0))
}

/// World coordinates in [0, 1] → longitude/latitude.
pub fn world_to_lonlat(x: f64, y: f64) -> (f64, f64) {
    let lon = x.rem_euclid(1.0) * 360.0 - 180.0;
    let n = std::f64::consts::PI * (1.0 - 2.0 * y.clamp(0.0, 1.0));
    let lat = n.sinh().atan().to_degrees();
    (lon, lat)
}

/// Tiles covering a viewport centred on `center` (world coordinates) at
/// integer `zoom`, with each tile's top-left corner in screen pixels.
/// Columns wrap around the antimeridian; rows outside the world are skipped.
pub fn visible_tiles(center: (f64, f64), zoom: u8, width: f32, height: f32) -> Vec<(TileId, f32, f32)> {
    let zoom = zoom.min(MAX_ZOOM);
    let n = 1u32 << zoom;
    let world_px = f64::from(n) * f64::from(TILE_SIZE);
    let (cx, cy) = (center.0 * world_px, center.1 * world_px);
    let left = cx - f64::from(width) / 2.0;
    let top = cy - f64::from(height) / 2.0;
    let tile = f64::from(TILE_SIZE);
    // Right and bottom edges are exclusive: a view ending on a tile
    // boundary must not request the next tile.
    let (x0, x1) = ((left / tile).floor() as i64, ((left + f64::from(width)) / tile).ceil() as i64 - 1);
    let (y0, y1) = ((top / tile).floor() as i64, ((top + f64::from(height)) / tile).ceil() as i64 - 1);
    let mut out = Vec::new();
    for ty in y0.max(0)..=y1.min(i64::from(n) - 1) {
        for tx in x0..=x1 {
            let wrapped = tx.rem_euclid(i64::from(n)) as u32;
            let sx = (tx as f64 * tile - left) as f32;
            let sy = (ty as f64 * tile - top) as f32;
            out.push((TileId { z: zoom, x: wrapped, y: ty as u32 }, sx, sy));
        }
    }
    out
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TileError {
    #[error("tile URL must be https and contain {{z}}, {{x}} and {{y}}")]
    BadTemplate,
    #[error("tile request failed: {0}")]
    Http(String),
    #[error("tile server returned {0}")]
    Status(u16),
    #[error("tile server returned something that is not an image")]
    NotImage,
    #[error("tile is too large")]
    TooLarge,
    #[error("tile cache error: {0}")]
    Cache(String),
}

/// Validates a tile URL template.
pub fn validate_template(t: &str) -> Result<(), TileError> {
    let ok = t.starts_with("https://") && t.contains("{z}") && t.contains("{x}") && t.contains("{y}");
    if ok { Ok(()) } else { Err(TileError::BadTemplate) }
}

pub fn tile_url(template: &str, id: TileId) -> String {
    template.replace("{z}", &id.z.to_string()).replace("{x}", &id.x.to_string()).replace("{y}", &id.y.to_string())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileMeta {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Unix seconds until which the tile may be used without revalidation.
    pub fresh_until: u64,
}

/// Freshness lifetime from response headers: `Cache-Control: max-age`
/// (`no-cache`/`no-store` → revalidate every time), then `Expires`,
/// otherwise the 7-day policy minimum.
pub fn freshness(cache_control: Option<&str>, expires: Option<&str>, now: u64) -> u64 {
    if let Some(cc) = cache_control {
        let cc = cc.to_ascii_lowercase();
        if cc.split(',').any(|d| matches!(d.trim(), "no-cache" | "no-store")) {
            return now;
        }
        if let Some(age) =
            cc.split(',').find_map(|d| d.trim().strip_prefix("max-age=").and_then(|v| v.trim().parse::<u64>().ok()))
        {
            return now + age;
        }
    }
    if let Some(exp) = expires.and_then(httpdate_parse) {
        return exp.max(now);
    }
    now + DEFAULT_FRESHNESS.as_secs()
}

/// Parses an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`).
fn httpdate_parse(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc2822(s.trim()).ok().map(|d| d.timestamp().max(0) as u64)
}

/// On-disk tile cache: `<dir>/<z>/<x>/<y>.png` plus a `.json` metadata file.
#[derive(Debug, Clone)]
pub struct TileCache {
    dir: PathBuf,
}

impl TileCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn paths(&self, id: TileId) -> (PathBuf, PathBuf) {
        let base = self.dir.join(id.z.to_string()).join(id.x.to_string());
        (base.join(format!("{}.png", id.y)), base.join(format!("{}.json", id.y)))
    }

    pub fn read(&self, id: TileId) -> Option<(Vec<u8>, TileMeta)> {
        let (img, meta) = self.paths(id);
        let bytes = std::fs::read(img).ok()?;
        let meta: TileMeta = serde_json::from_slice(&std::fs::read(meta).ok()?).ok()?;
        Some((bytes, meta))
    }

    pub fn write(&self, id: TileId, bytes: &[u8], meta: &TileMeta) -> Result<(), TileError> {
        let (img, m) = self.paths(id);
        if let Some(parent) = img.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TileError::Cache(e.to_string()))?;
        }
        std::fs::write(&img, bytes).map_err(|e| TileError::Cache(e.to_string()))?;
        self.write_meta(&m, meta)
    }

    fn write_meta(&self, path: &Path, meta: &TileMeta) -> Result<(), TileError> {
        std::fs::write(path, serde_json::to_vec(meta).unwrap_or_default()).map_err(|e| TileError::Cache(e.to_string()))
    }

    pub fn refresh(&self, id: TileId, meta: &TileMeta) -> Result<(), TileError> {
        self.write_meta(&self.paths(id).1, meta)
    }

    /// Total size of cached tiles in bytes.
    pub fn size(&self) -> u64 {
        fn walk(p: &Path) -> u64 {
            std::fs::read_dir(p)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| {
                            let path = e.path();
                            if path.is_dir() { walk(&path) } else { e.metadata().map(|m| m.len()).unwrap_or(0) }
                        })
                        .sum()
                })
                .unwrap_or(0)
        }
        walk(&self.dir)
    }

    pub fn clear(&self) -> Result<(), TileError> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(TileError::Cache(e.to_string())),
        }
    }
}

/// Fetches tiles through the cache.
#[derive(Clone)]
pub struct TileClient {
    http: reqwest::Client,
    template: String,
    cache: TileCache,
    slots: Arc<Semaphore>,
}

impl TileClient {
    pub fn new(template: &str, cache: TileCache) -> Result<Self, TileError> {
        validate_template(template)?;
        let http = reqwest::Client::builder()
            .user_agent(crate::USER_AGENT)
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| TileError::Http(e.to_string()))?;
        Ok(Self { http, template: template.to_string(), cache, slots: Arc::new(Semaphore::new(MAX_PARALLEL)) })
    }

    /// For tests: a client for a non-https mock server.
    #[doc(hidden)]
    pub fn new_unchecked(template: &str, cache: TileCache) -> Self {
        let http = reqwest::Client::builder().user_agent(crate::USER_AGENT).build().expect("client");
        Self { http, template: template.to_string(), cache, slots: Arc::new(Semaphore::new(MAX_PARALLEL)) }
    }

    /// Returns tile bytes from the cache when fresh; otherwise fetches or
    /// revalidates. A stale cached tile is returned if the network fails.
    pub async fn tile(&self, id: TileId) -> Result<Vec<u8>, TileError> {
        let now = now_secs();
        let cached = self.cache.read(id);
        if let Some((bytes, meta)) = &cached
            && meta.fresh_until > now
        {
            return Ok(bytes.clone());
        }
        let _permit = self.slots.acquire().await.map_err(|e| TileError::Http(e.to_string()))?;
        let mut req = self.http.get(tile_url(&self.template, id));
        if let Some((_, meta)) = &cached {
            if let Some(e) = &meta.etag {
                req = req.header(IF_NONE_MATCH, e);
            }
            if let Some(lm) = &meta.last_modified {
                req = req.header(IF_MODIFIED_SINCE, lm);
            }
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return cached.map(|(b, _)| b).ok_or_else(|| TileError::Http(e.to_string())),
        };
        let header = |n| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
        let (cc, exp, etag, lm, ctype) =
            (header(CACHE_CONTROL), header(EXPIRES), header(ETAG), header(LAST_MODIFIED), header(CONTENT_TYPE));
        let fresh_until = freshness(cc.as_deref(), exp.as_deref(), now);
        let no_store = cc.as_deref().is_some_and(|c| c.to_ascii_lowercase().contains("no-store"));
        match resp.status() {
            StatusCode::NOT_MODIFIED => {
                let (bytes, mut meta) = cached.ok_or(TileError::Status(304))?;
                meta.fresh_until = fresh_until;
                if etag.is_some() {
                    meta.etag = etag;
                }
                self.cache.refresh(id, &meta)?;
                Ok(bytes)
            }
            StatusCode::OK => {
                if !ctype.as_deref().is_some_and(|c| c.starts_with("image/")) {
                    return Err(TileError::NotImage);
                }
                if resp.content_length().is_some_and(|l| l as usize > MAX_TILE_BYTES) {
                    return Err(TileError::TooLarge);
                }
                let bytes = resp.bytes().await.map_err(|e| TileError::Http(e.to_string()))?;
                if bytes.len() > MAX_TILE_BYTES {
                    return Err(TileError::TooLarge);
                }
                if !no_store {
                    self.cache.write(id, &bytes, &TileMeta { etag, last_modified: lm, fresh_until })?;
                }
                Ok(bytes.to_vec())
            }
            other => cached.map(|(b, _)| b).ok_or(TileError::Status(other.as_u16())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mercator_round_trip_and_known_points() {
        let (x, y) = lonlat_to_world(0.0, 0.0);
        assert!((x - 0.5).abs() < 1e-12 && (y - 0.5).abs() < 1e-12);
        for (lon, lat) in [(24.94, 60.17), (-74.0, 40.7), (151.2, -33.9)] {
            let (x, y) = lonlat_to_world(lon, lat);
            let (lo, la) = world_to_lonlat(x, y);
            assert!((lo - lon).abs() < 1e-9 && (la - lat).abs() < 1e-9, "{lon},{lat}");
        }
        // Helsinki is in tile (9, 291, 148) at zoom 9.
        let (x, y) = lonlat_to_world(24.94, 60.17);
        assert_eq!(((x * 512.0) as u32, (y * 512.0) as u32), (291, 148));
        assert_eq!(lonlat_to_world(10.0, 89.9).1, 0.0, "poles clamp to the Mercator limit");
    }

    #[test]
    fn visible_tiles_cover_only_the_viewport() {
        let tiles = visible_tiles((0.5, 0.5), 3, 512.0, 512.0);
        assert_eq!(tiles.len(), 4, "a 512px view centred on a tile corner needs exactly 2x2 tiles");
        assert!(tiles.iter().all(|(t, _, _)| t.z == 3 && t.x < 8 && t.y < 8));
        // Near the antimeridian columns wrap instead of going negative.
        let wrap = visible_tiles((0.001, 0.5), 2, 600.0, 256.0);
        assert!(wrap.iter().any(|(t, _, _)| t.x == 3));
        // Zoom 0 at a large window: just the single world tile (rows don't repeat).
        assert_eq!(visible_tiles((0.5, 0.5), 0, 256.0, 2000.0).iter().filter(|(t, ..)| t.x == 0).count(), 1);
    }

    #[test]
    fn templates_must_be_https_with_placeholders() {
        assert!(validate_template(OSM_TILE_URL).is_ok());
        assert!(validate_template("http://tile.openstreetmap.org/{z}/{x}/{y}.png").is_err());
        assert!(validate_template("https://example.com/tiles.png").is_err());
        assert_eq!(tile_url(OSM_TILE_URL, TileId { z: 3, x: 4, y: 5 }), "https://tile.openstreetmap.org/3/4/5.png");
    }

    #[test]
    fn freshness_rules() {
        assert_eq!(freshness(Some("max-age=3600, public"), None, 100), 3700);
        assert_eq!(freshness(Some("no-cache"), None, 100), 100);
        assert_eq!(freshness(None, Some("Sun, 06 Nov 1994 08:49:37 GMT"), 0), 784_111_777);
        assert_eq!(freshness(None, None, 0), 7 * 24 * 3600);
        assert_eq!(freshness(Some("public"), Some("garbage"), 10), 10 + 7 * 24 * 3600);
    }
}
