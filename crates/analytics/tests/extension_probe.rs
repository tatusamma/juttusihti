//! Opt-in feasibility probe: can the DuckDB `sqlite` extension be installed
//! for the bundled DuckDB build? Needs network; the app never does this.
//! Run with: cargo test -p juttusihti-analytics --test extension_probe -- --ignored --nocapture

#[test]
#[ignore = "downloads from extensions.duckdb.org"]
fn probe_sqlite_extension_install() {
    let dir = tempfile::tempdir().unwrap();
    let ext_dir = dir.path().join("ext").to_string_lossy().replace('\\', "/");
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!("SET extension_directory = '{ext_dir}'")).unwrap();
    let version: String = conn.query_row("SELECT version()", [], |r| r.get(0)).unwrap();
    match conn.execute_batch("INSTALL sqlite; LOAD sqlite;") {
        Ok(()) => println!("duckdb {version}: sqlite extension installed and loaded"),
        Err(e) => println!("duckdb {version}: sqlite extension unavailable: {e}"),
    }
}
