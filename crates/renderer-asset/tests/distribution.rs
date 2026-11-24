use renderer_asset::{
    AssetId, BlobRef, Catalog, Digest, Equipment, Generation, ImageBlob, MeshBlob, ObjectStore,
    Primitive,
};
use std::{
    fs, io,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
};

fn image(value: u8) -> ImageBlob {
    ImageBlob {
        width: 1,
        height: 1,
        mip_levels: 1,
        srgb: true,
        pixels: vec![value; 4],
    }
}

#[test]
fn concurrent_requests_share_bytes_and_decode_until_last_owner() -> io::Result<()> {
    let source = image(17).encode()?;
    let object = BlobRef::of(&source);
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let store = ObjectStore::with_reader(".", move |path, size| {
        assert!(path.ends_with(object.hash.to_string()));
        assert_eq!(size, object.size);
        counter.fetch_add(1, Relaxed);
        Ok(source.clone())
    });
    let start = Barrier::new(4);
    let owners = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    let bytes = store.load(&object)?;
                    assert_eq!(bytes.image()?, &image(17));
                    Ok(bytes)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<io::Result<Vec<_>>>()
    })?;
    assert_eq!(reads.load(Relaxed), 1);
    for other in &owners[1..] {
        assert!(Arc::ptr_eq(&owners[0], other));
        assert!(std::ptr::eq(owners[0].image()?, other.image()?));
    }
    assert!(
        store
            .load(&BlobRef {
                size: object.size + 1,
                ..object
            })
            .is_err()
    );
    assert!(owners[0].mesh().is_err());
    #[cfg(feature = "metrics")]
    assert_eq!(store.stats().live_bytes, object.size + 4);
    drop(owners);
    #[cfg(feature = "metrics")]
    assert_eq!(store.stats().live_bytes, 0);
    drop(store.load(&object)?);
    assert_eq!(reads.load(Relaxed), 2);
    Ok(())
}

#[test]
fn rejected_contents_are_retried_for_the_same_digest() -> io::Result<()> {
    let source = image(7).encode()?;
    let object = BlobRef::of(&source);
    let reads = AtomicUsize::new(0);
    let store = ObjectStore::with_reader(".", move |_, _| {
        let mut bytes = source.clone();
        if reads.fetch_add(1, Relaxed) == 0 {
            *bytes.last_mut().unwrap() ^= 1;
        }
        Ok(bytes)
    });
    assert!(store.load(&object).is_err());
    assert_eq!(store.load(&object)?.image()?, &image(7));
    Ok(())
}

#[test]
fn rejects_truncated_overflowing_and_invalid_payloads() -> io::Result<()> {
    let mut bytes = image(9).encode()?;
    bytes.pop();
    assert!(ImageBlob::decode(&bytes).is_err());
    bytes[8..16].fill(255);
    assert!(ImageBlob::decode(&bytes).is_err());
    let mesh = MeshBlob {
        vertices: vec![[0.; 12]; 3],
        indices: vec![0, 1, 2],
    };
    let mut data = mesh.encode()?;
    assert_eq!(MeshBlob::decode(&data)?, mesh);
    let idx = data.len() - 4;
    data[idx..].copy_from_slice(&3u32.to_le_bytes());
    assert!(MeshBlob::decode(&data).is_err());
    Ok(())
}

#[test]
fn rejected_generation_preserves_a_pinned_equipment_selection() -> io::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "renderer-asset-generation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root)?;
    fs::create_dir(root.join("catalogs"))?;
    let id = AssetId::from_u128(12);
    let obj = BlobRef::of(b"sample");
    let equipment = Equipment {
        primitives: vec![Primitive {
            mesh: obj,
            material: obj,
            images: [None; 4],
        }],
    };
    let mut catalog = Catalog {
        version: 1,
        assets: [(id, equipment.clone())].into_iter().collect(),
    };
    let publish = |bytes: Vec<u8>| -> io::Result<Digest> {
        let digest = Digest::of(&bytes);
        fs::write(root.join("catalogs").join(digest.to_string()), bytes)?;
        fs::write(root.join("current"), digest.to_string())?;
        Ok(digest)
    };
    let before = publish(serde_json::to_vec(&catalog)?)?;
    let pinned = Generation::open(&root)?;
    catalog.version = 2;
    let unknown = publish(serde_json::to_vec(&catalog)?)?;
    assert!(Generation::open(&root).is_err());
    fs::write(root.join("catalogs").join(unknown.to_string()), b"other")?;
    assert!(Generation::open(&root).is_err());
    let e = serde_json::to_string(&equipment)?;
    publish(format!(r#"{{"version":1,"assets":{{"{id}":{e},"{id}":{e}}}}}"#).into_bytes())?;
    assert!(Generation::open(&root).is_err());
    assert_eq!(pinned.id(), &before);
    assert_eq!(pinned.equipment(&id)?, &equipment);
    assert_eq!(
        Generation::open_id(&root, before)?.equipment(&id)?,
        &equipment
    );
    fs::remove_dir_all(root)?;
    Ok(())
}
