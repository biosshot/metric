use metric_domain::{ProjectId, SecretBytes, Timestamp};
use metric_mongo::{EventCodecConfig, MetricRetention, MongoProjectStore};
use mongodb::{
    Database,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
};
use std::{
    error::Error,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[tokio::test]
#[ignore = "requires Docker MongoDB"]
async fn fresh_metric_archive_lifecycle_validators_and_native_ttl_preserve_holds() {
    let client = mongodb::Client::with_uri_str(std::env::var("METRIC_TEST_MONGODB_URI").unwrap())
        .await
        .unwrap();
    let database = client.database(&format!(
        "metric_pg_metric_archive_lifecycle_{}",
        mongodb::bson::oid::ObjectId::new()
    ));
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}
async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let at = |v| Timestamp::from_unix_millis(v).unwrap();
    let archive = control.archive_store(EventCodecConfig::default());
    let metrics = control.metric_store(MetricRetention {
        days: 1,
        max_series_per_project: 16,
        archive: true,
    });
    let claimed = metric_testkit::archive_conformance::metric_archive_lifecycle(
        &metrics,
        &archive,
        ProjectId::new(42)?,
        at(now),
        at(now + 86_400_000),
    )
    .await;
    let collection = database.collection::<Document>("metric_buckets");
    let id = Binary {
        subtype: BinarySubtype::Generic,
        bytes: claimed.source_ids[0].as_bytes(),
    };
    let committed = collection
        .find_one(doc! {"_id":&id})
        .await?
        .ok_or("missing committed source")?;
    assert_eq!(
        committed.get_binary_generic("z")?.as_slice(),
        claimed.segment_id.as_bytes()
    );
    assert_eq!(
        committed.get_datetime("x")?.timestamp_millis(),
        now + 86_400_000
    );
    assert!(!committed.contains_key("h"));
    assert_eq!(committed.get_f64("s0")?, 2.0);
    assert_eq!(committed.get_i64("c")?, 1);
    for (seed, z, x, h) in [
        (
            90,
            Some::<Bson>(binary(9, 15).into()),
            Some::<Bson>(DateTime::from_millis(now).into()),
            None::<Bson>,
        ),
        (91, Some(binary(9, 16).into()), None, None),
        (
            92,
            Some(binary(9, 16).into()),
            Some("wrong-date".into()),
            None,
        ),
        (
            93,
            Some(binary(9, 16).into()),
            Some(DateTime::from_millis(now).into()),
            Some(DateTime::from_millis(now).into()),
        ),
    ] {
        let mut invalid = committed.clone();
        invalid.insert("_id", binary(seed, 16));
        invalid.remove("z");
        invalid.remove("x");
        if let Some(z) = z {
            invalid.insert("z", z);
        }
        if let Some(x) = x {
            invalid.insert("x", x);
        }
        if let Some(h) = h {
            invalid.insert("h", h);
        }
        let error = collection.insert_one(invalid).await.unwrap_err();
        assert!(
            matches!(error.kind.as_ref(),mongodb::error::ErrorKind::Write(mongodb::error::WriteFailure::WriteError(e)) if e.code==121)
        );
    }
    let mut ordinary = committed.clone();
    ordinary.insert("_id", binary(70, 16));
    ordinary.insert("z", DateTime::from_millis(now + 86_400_000));
    // Legacy ordinary records must not be captured by the new x TTL index.
    ordinary.insert("x", DateTime::from_millis(now - 86_400_000));
    collection.insert_one(ordinary.clone()).await?;
    let mut held = committed.clone();
    held.insert("_id", binary(71, 16));
    held.remove("z");
    held.remove("x");
    held.insert("h", DateTime::from_millis(now - 86_400_000));
    collection.insert_one(held.clone()).await?;
    let mut expired_ordinary = ordinary.clone();
    expired_ordinary.insert("_id", binary(72, 16));
    expired_ordinary.remove("x");
    expired_ordinary.insert("z", DateTime::from_millis(now - 86_400_000));
    collection.insert_one(expired_ordinary).await?;
    collection
        .update_one(
            doc! {"_id":&id},
            doc! {"$set":{"x":DateTime::from_millis(now-86_400_000)}},
        )
        .await?;
    // Observe the actual TTL monitor without altering its global schedule.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let count = collection
            .count_documents(doc! {"_id":{"$in":[id.clone(),binary(72,16)]}})
            .await?;
        if count == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native metric TTL did not remove expired copies within 90 seconds"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        collection
            .find_one(doc! {"_id":binary(70,16)})
            .await?
            .unwrap(),
        ordinary
    );
    assert_eq!(
        collection
            .find_one(doc! {"_id":binary(71,16)})
            .await?
            .unwrap(),
        held
    );
    assert!(metric_ports::ArchiveStore::object_referenced(&archive, &claimed.object_key).await?);
    Ok(())
}
fn binary(seed: u8, len: usize) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: vec![seed; len],
    }
}
