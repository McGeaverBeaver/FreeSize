//! Comparing scans, searching across devices, exports, and devices without
//! an agent whose scans are uploaded as files.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use common::idp::MockUser;
use common::{Browser, blocking, start_hub};
use freesize_agent::{Client, UreqTransport, pair};
use freesize_core::{ScanOptions, ScanProgress, scan};
use freesize_proto::{ActionKind, DeviceInfo, PairingLink, Platform, Report};
use serde_json::json;

fn info(name: &str) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform: Platform::Linux,
        os: "Debian 13".into(),
        hostname: "nas".into(),
        app: "FreeSize agent test".into(),
        actions: vec![ActionKind::Delete],
        attended: false,
        approver: None,
        grants: vec![],
        rights: String::new(),
    }
}

fn write(dir: &Path, path: &str, bytes: usize) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![7u8; bytes]).unwrap();
}

fn scan_report(dir: &Path, taken: i64) -> Report {
    let tree = scan(dir, &ScanOptions::default(), &ScanProgress::new())
        .unwrap()
        .tree;
    Report {
        info: None,
        disks: None,
        scan: Some(freesize_agent::report::scan_report(
            &tree, "Shares", taken, 0, true,
        )),
        ..Report::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn compare_search_export_and_upload() {
    let hub = start_hub("reports").await;
    let url = hub.url.clone();
    let idp = hub.idp.clone();
    let setup = url.clone();
    let (search_id, router) = blocking(move || {
        let url = setup;
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let code = admin
            .post(
                &format!("{url}/api/pairing-codes"),
                &json!({"group_id": 1}),
                true,
            )
            .json();
        let link = PairingLink::parse(code["link"].as_str().unwrap()).unwrap();
        let transport = UreqTransport::new();
        let state = pair(&transport, &link, info("NAS"), &mut |_| false).unwrap();
        let client = Client::new(&state, &transport);

        // Two scans of a folder, a month apart: Movies grows, Backups goes,
        // Photos is new.
        let dir = std::env::temp_dir().join(format!("freesize-hub-reports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "Movies/a.mkv", 3_000_000);
        write(&dir, "Backups/old.tar", 4_000_000);
        write(&dir, "ISOs/ubuntu-24.04.iso", 2_000_000);
        client.report(&scan_report(&dir, 1_780_000_000)).unwrap();
        write(&dir, "Movies/b.mkv", 5_000_000);
        std::fs::remove_dir_all(dir.join("Backups")).unwrap();
        write(&dir, "Photos/x.jpg", 2_000_000);
        client.report(&scan_report(&dir, 1_782_600_000)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        let devices = admin.get(&format!("{url}/api/devices")).json();
        let nas = devices[0]["id"].as_str().unwrap().to_string();
        let detail = admin.get(&format!("{url}/api/devices/{nas}")).json();
        let scans = &detail["roots"][0]["scans"];
        let (newest, oldest) = (scans[0]["id"].as_i64().unwrap(), scans[1]["id"].as_i64().unwrap());

        // Either order compares the older scan with the newer.
        let compared = admin
            .get(&format!("{url}/api/scans/{newest}/compare?with={oldest}"))
            .json();
        assert_eq!(compared["from"]["id"], oldest);
        assert_eq!(compared["to"]["id"], newest);
        let kinds: HashMap<String, String> = compared["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["path"].as_str().unwrap().to_string(),
                    c["kind"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(kinds["Movies"], "grew", "{kinds:?}");
        assert_eq!(kinds["Backups"], "gone");
        assert_eq!(kinds["Photos"], "new");
        assert!(compared["grew"].as_u64().unwrap() >= 7_000_000);

        let csv = admin.get(&format!(
            "{url}/api/scans/{oldest}/compare?with={newest}&format=csv"
        ));
        assert_eq!(csv.status, 200);
        assert!(csv.header("content-type").starts_with("text/csv"));
        assert!(
            csv.header("content-disposition")
                .contains("NAS-Shares-2026-05-28-to-2026-06-27.csv"),
            "{}",
            csv.header("content-disposition")
        );
        assert!(csv.body.starts_with("change,path,before,after,difference\r\n"));
        assert!(csv.body.contains("\r\ngone,Backups,"), "{}", csv.body);

        // Search: words in the path, file types, and only the top match.
        let found = admin.get(&format!("{url}/api/search?q=ubuntu")).json();
        assert_eq!(found["hits"][0]["path"], "ISOs/ubuntu-24.04.iso");
        assert_eq!(found["hits"][0]["kind"], "file");
        assert_eq!(found["hits"][0]["device_name"], "NAS");
        let isos = admin.get(&format!("{url}/api/search?q=*.iso")).json();
        assert_eq!(isos["hits"].as_array().unwrap().len(), 1);
        let movies = admin.get(&format!("{url}/api/search?q=movies")).json();
        let paths: Vec<&str> = movies["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["Movies"]);
        let big = admin
            .get(&format!("{url}/api/search?q=movies&min=100000000"))
            .json();
        assert!(big["hits"].as_array().unwrap().is_empty());
        assert_eq!(admin.get(&format!("{url}/api/search?q=%20")).status, 400);

        let saved = admin.post(
            &format!("{url}/api/searches"),
            &json!({"name": "ISOs", "query": "*.iso", "min": 0}),
            true,
        );
        assert_eq!(saved.status, 200, "{}", saved.body);
        let search_id = saved.json()["id"].as_i64().unwrap();
        assert_eq!(
            admin.get(&format!("{url}/api/searches")).json()[0]["query"],
            "*.iso"
        );

        // Exports.
        let scan_csv = admin.get(&format!("{url}/api/scans/{newest}/export?format=csv"));
        assert!(scan_csv.body.starts_with("type,path,size,on disk,changed\r\n"));
        assert!(scan_csv.body.contains("\r\nfolder,Movies,"));
        assert!(scan_csv.body.contains("\r\nfile,ISOs/ubuntu-24.04.iso,"));
        let scan_json = admin
            .get(&format!("{url}/api/scans/{newest}/export?format=json"))
            .json();
        assert_eq!(scan_json["scan"]["id"], newest);
        assert_eq!(scan_json["device"]["name"], "NAS");
        let devices_csv = admin.get(&format!("{url}/api/devices/export"));
        assert!(devices_csv.body.contains("\r\nNAS,Devices,linux,Debian 13,"));

        // A device without an agent: made by an admin, fed with ncdu files.
        let made = admin.post(
            &format!("{url}/api/devices"),
            &json!({"name": "Router <b>", "group_id": 1, "platform": "linux"}),
            true,
        );
        assert_eq!(made.status, 200, "{}", made.body);
        let router = made.json()["id"].as_str().unwrap().to_string();
        let ncdu = br#"[1,2,{"progname":"ncdu","progver":"1.19","timestamp":1781000000},
            [{"name":"/var","asize":4096,"dsize":4096},
             [{"name":"log","asize":4096,"dsize":4096},
              {"name":"syslog","asize":3000000,"dsize":3002368},
              {"name":"secret","read_error":true}],
             {"name":"swapfile","asize":1000000,"dsize":1003520}]]"#;
        let mut packed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        packed.write_all(ncdu).unwrap();
        let packed = packed.finish().unwrap();
        let uploaded = admin.post_bytes(
            &format!("{url}/api/devices/{router}/scans?label=Router%20logs"),
            &packed,
        );
        assert_eq!(uploaded.status, 200, "{}", uploaded.body);
        let uploaded = uploaded.json();
        assert_eq!(uploaded["root"], "/var");
        assert_eq!(uploaded["label"], "Router logs");
        assert_eq!(uploaded["taken"], 1_781_000_000);
        let detail = admin.get(&format!("{url}/api/devices/{router}")).json();
        assert_eq!(detail["agentless"], true);
        assert_eq!(detail["online"], false);
        assert_eq!(detail["roots"][0]["scans"][0]["errors"], 1);
        let syslog = admin.get(&format!("{url}/api/search?q=syslog")).json();
        assert_eq!(syslog["hits"][0]["device_id"], router.as_str());
        assert_eq!(syslog["hits"][0]["path"], "log/syslog");

        // Agents send their own scans; files that aren't scans are refused.
        let to_agent = admin.post_bytes(&format!("{url}/api/devices/{nas}/scans"), ncdu);
        assert_eq!(to_agent.status, 400);
        let junk = admin.post_bytes(&format!("{url}/api/devices/{router}/scans"), b"hello");
        assert_eq!(junk.status, 400);
        assert!(junk.json()["error"].as_str().unwrap().contains("not a saved scan"));
        // Nothing runs there: it can't be asked to delete.
        let ask = admin.post(
            &format!("{url}/api/devices/{router}/commands"),
            &json!({"action": {"type": "delete", "root": "/var", "paths": ["log/syslog"], "bytes": 0}}),
            true,
        );
        assert_eq!(ask.status, 400, "{}", ask.body);

        // The printable report stands alone and escapes what devices say.
        let report = admin.get(&format!("{url}/api/devices/{router}/report"));
        assert_eq!(report.status, 200);
        assert!(report.header("content-security-policy").starts_with("default-src 'none'"));
        assert!(report.body.contains("<h1>Router &lt;b&gt;</h1>"));
        assert!(report.body.contains("log/syslog"));
        let saved = admin.get(&format!("{url}/api/devices/{router}/report?download=true"));
        assert!(saved.header("content-disposition").contains("Router-b-report-"));

        let log = admin.get(&format!("{url}/api/devices/{router}/audit")).json();
        let actions: Vec<&str> = log
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap())
            .collect();
        assert!(actions.contains(&"device added without an agent"));
        assert!(actions.contains(&"scan file uploaded"));
        (search_id, router)
    })
    .await;

    // Viewers can't add devices or upload, nor touch other people's saved
    // searches.
    idp.set_user(MockUser::new("u-viewer", "Val", &["freesize-viewers"]));
    blocking(move || {
        let mut viewer = Browser::new();
        assert_eq!(viewer.sign_in(&url, "%2F").status, 303);
        let made = viewer.post(
            &format!("{url}/api/devices"),
            &json!({"name": "Mine", "group_id": 1}),
            true,
        );
        assert_eq!(made.status, 403);
        let upload = viewer.post_bytes(&format!("{url}/api/devices/{router}/scans"), b"[1,2]");
        assert_eq!(upload.status, 403);
        assert!(
            viewer
                .get(&format!("{url}/api/searches"))
                .json()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            viewer
                .delete(&format!("{url}/api/searches/{search_id}"))
                .status,
            404
        );
        // But they can search what they may see.
        let found = viewer.get(&format!("{url}/api/search?q=syslog")).json();
        assert_eq!(found["hits"].as_array().unwrap().len(), 1);
    })
    .await;
}
