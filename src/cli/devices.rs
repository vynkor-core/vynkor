use serde::Deserialize;

/// The kernel's device record as served by GET /devices (D-04). Fields mirror
/// `api::routes::DeviceInfoView` minus `os_version`, which the table doesn't
/// display; keep the rest in sync.
#[derive(Deserialize)]
struct DeviceView {
    device_id: String,
    os: String,
    arch: String,
    capabilities: Vec<String>,
    last_seen: u64,
    state: String,
}

/// `vyn devices` — list devices the kernel has ever seen, via GET /devices.
pub async fn handle(port: u16, tls: bool, token: Option<&str>) -> anyhow::Result<()> {
    let scheme = if tls { "https" } else { "http" };
    let base = format!("{scheme}://127.0.0.1:{port}");
    let body = super::plugin::api_get(&base, "/devices", token).await?;
    let devices: Vec<DeviceView> = serde_json::from_str(&body)?;
    print_table(&devices);
    Ok(())
}

fn device_row(d: &DeviceView) -> [String; 6] {
    [
        d.device_id.clone(),
        d.os.clone(),
        d.arch.clone(),
        d.state.clone(),
        crate::marketplace::state::format_ts(d.last_seen / 1000),
        d.capabilities.join(", "),
    ]
}

fn print_table(devices: &[DeviceView]) {
    if devices.is_empty() {
        println!("No devices seen. Plugins that declare a device_id appear here.");
        return;
    }

    const HEADERS: [&str; 6] = [
        "DEVICE_ID",
        "OS",
        "ARCH",
        "STATE",
        "LAST_SEEN",
        "CAPABILITIES",
    ];
    let mut widths: [usize; 6] = HEADERS.map(str::len);
    let rows: Vec<[String; 6]> = devices.iter().map(device_row).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    println!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}",
        HEADERS[0],
        HEADERS[1],
        HEADERS[2],
        HEADERS[3],
        HEADERS[4],
        HEADERS[5],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
        w4 = widths[4],
    );
    for row in &rows {
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            row[5],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_row_formats_capabilities_and_timestamp() {
        let d = DeviceView {
            device_id: "phone-1".to_string(),
            os: "android".to_string(),
            arch: "aarch64".to_string(),
            capabilities: vec!["geo".to_string(), "battery".to_string()],
            last_seen: 1_700_000_000_000,
            state: "online".to_string(),
        };
        let row = device_row(&d);
        assert_eq!(row[0], "phone-1");
        assert_eq!(row[1], "android");
        assert_eq!(row[2], "aarch64");
        assert_eq!(row[3], "online");
        assert_eq!(row[5], "geo, battery");
    }
}
