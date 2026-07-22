use std::{collections::BTreeSet, num::NonZeroU64, time::Instant};

use faultkeep_domain::{
    EventId, ProjectId, Timestamp,
    grouping::{
        GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
        GroupingStrategy, IssueId, verify_issue_id,
    },
    issue::{
        ActorRef, IssueCommand, IssueCommandAction, IssueCommandResult, IssueCulprit,
        IssueGroupingDetail, IssueMutationKind, IssueMutationResult, IssueOccurrence, IssueRelease,
        IssueSearchQuery, IssueSearchResult, IssueSnapshot, IssueStatus, IssueTitle, IssueWorkflow,
        RegressionSummary, command_activity_id, regression_activity_id,
    },
};
use faultkeep_ports::{IssueStore, IssueStoreError, PortFuture};
use futures_util::TryStreamExt;
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::{Error as MongoError, ErrorKind, WriteFailure},
    options::{Collation, IndexOptions, ReturnDocument},
};
use serde_json::{Value, json};
use thiserror::Error;

const BODY_FORMAT_VERSION: u8 = 1;
const BODY_CODEC_JSON: u8 = 0;
const BODY_CODEC_ZSTD: u8 = 1;
const DUPLICATE_KEY_CODE: i32 = 11000;

#[derive(Debug, Clone, Copy)]
pub struct IssueCodecConfig {
    pub compression_level: i32,
    pub compression_min_savings: usize,
    pub max_decoded_body_bytes: usize,
    pub max_encoded_document_bytes: usize,
}

impl Default for IssueCodecConfig {
    fn default() -> Self {
        Self {
            compression_level: 3,
            compression_min_savings: 64,
            max_decoded_body_bytes: 64 * 1024,
            max_encoded_document_bytes: 128 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IssueCodecError {
    #[error("Issue BSON uses an unknown or malformed physical format")]
    InvalidDocument,
    #[error("Issue grouping body uses an unknown or malformed codec")]
    InvalidBody,
    #[error("Issue grouping body exceeds the configured size bound")]
    TooLarge,
}

#[derive(Clone)]
pub struct MongoIssueStore {
    database: Database,
    codec: IssueCodecConfig,
}

impl MongoIssueStore {
    #[must_use]
    pub const fn from_database(database: Database, codec: IssueCodecConfig) -> Self {
        Self { database, codec }
    }

    #[must_use]
    pub const fn codec_config(&self) -> IssueCodecConfig {
        self.codec
    }

    async fn apply_occurrence_inner(
        &self,
        occurrence: IssueOccurrence,
    ) -> Result<IssueMutationResult, IssueStoreError> {
        if !verify_issue_id(
            occurrence.project_id,
            occurrence.grouping_key,
            occurrence.issue_id,
        ) {
            return Err(IssueStoreError::InvalidData);
        }
        let increment =
            i64::try_from(occurrence.increment.get()).map_err(|_| IssueStoreError::InvalidData)?;
        let body = encode_grouping_body(&occurrence.grouping, self.codec)
            .map_err(|_| IssueStoreError::InvalidData)?;
        let update = occurrence_pipeline(&occurrence, increment, body);
        let collection = self.database.collection::<Document>("issues");
        let result = collection
            .find_one_and_update(
                doc! {
                    "_id": binary(occurrence.issue_id.as_bytes()),
                    "p": occurrence.project_id.get(),
                    "g": binary(occurrence.grouping_key.to_bytes()),
                },
                update,
            )
            .upsert(true)
            .return_document(ReturnDocument::After)
            .await;
        let document = match result {
            Ok(Some(document)) => document,
            Ok(None) => return Err(IssueStoreError::Unavailable),
            Err(error) if duplicate_write(&error) => {
                return Err(IssueStoreError::IdentityCollision);
            }
            Err(_) => return Err(IssueStoreError::Unavailable),
        };
        let issue =
            decode_issue(&document, self.codec).map_err(|_| IssueStoreError::InvalidData)?;
        let kind = if issue.occurrence_count == occurrence.increment {
            IssueMutationKind::Created
        } else if issue.status == IssueStatus::Open
            && issue.regression.as_ref().is_some_and(|regression| {
                regression.event_id == occurrence.event_id
                    && regression.at == occurrence.received_at
            })
        {
            self.insert_regression_activity(&occurrence).await;
            IssueMutationKind::Regressed
        } else {
            IssueMutationKind::Updated
        };
        Ok(IssueMutationResult { kind, issue })
    }

    async fn apply_command_inner(
        &self,
        command: IssueCommand,
    ) -> Result<IssueCommandResult, IssueStoreError> {
        let collection = self.database.collection::<Document>("issues");
        let id = binary(command.issue_id.as_bytes());
        let base = doc! { "_id": id.clone(), "p": command.project_id.get() };
        let (filter, update) = match command.action {
            IssueCommandAction::Resolve => (
                and_filter(base, doc! { "s": { "$exists": false } }),
                doc! { "$set": {
                    "s": 1_i32,
                    "w": { "t": date(command.at), "a": actor_binary(command.actor) },
                } },
            ),
            IssueCommandAction::Ignore => (
                and_filter(
                    base,
                    doc! { "$or": [
                        { "s": { "$exists": false } },
                        { "s": 1_i32 },
                    ] },
                ),
                doc! { "$set": {
                    "s": 2_i32,
                    "w": { "t": date(command.at), "a": actor_binary(command.actor) },
                } },
            ),
            IssueCommandAction::Reopen => (
                and_filter(base, doc! { "s": { "$in": [1_i32, 2_i32] } }),
                doc! { "$unset": { "s": "", "w": "" } },
            ),
            IssueCommandAction::Assign(Some(assignee)) => (
                and_filter(base, doc! { "a": { "$ne": actor_binary(assignee) } }),
                doc! { "$set": { "a": actor_binary(assignee) } },
            ),
            IssueCommandAction::Assign(None) => (
                and_filter(base, doc! { "a": { "$exists": true } }),
                doc! { "$unset": { "a": "" } },
            ),
        };
        let updated = collection
            .find_one_and_update(filter, update)
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| IssueStoreError::Unavailable)?;
        let (applied, document) = match updated {
            Some(document) => (true, document),
            None => {
                let document = collection
                    .find_one(doc! { "_id": id, "p": command.project_id.get() })
                    .await
                    .map_err(|_| IssueStoreError::Unavailable)?
                    .ok_or(IssueStoreError::NotFound)?;
                (false, document)
            }
        };
        let issue =
            decode_issue(&document, self.codec).map_err(|_| IssueStoreError::InvalidData)?;
        if applied {
            self.insert_command_activity(command).await;
        }
        Ok(IssueCommandResult { applied, issue })
    }

    async fn load_inner(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
    ) -> Result<IssueSnapshot, IssueStoreError> {
        let document = self
            .database
            .collection::<Document>("issues")
            .find_one(doc! { "_id": binary(issue_id.as_bytes()), "p": project_id.get() })
            .await
            .map_err(|_| IssueStoreError::Unavailable)?
            .ok_or(IssueStoreError::NotFound)?;
        decode_issue(&document, self.codec).map_err(|_| IssueStoreError::InvalidData)
    }

    async fn search_titles_inner(
        &self,
        project_id: ProjectId,
        query: IssueSearchQuery,
    ) -> Result<Vec<IssueSearchResult>, IssueStoreError> {
        let mut cursor = self
            .database
            .collection::<Document>("issues")
            .find(doc! {
                "p": project_id.get(),
                "$text": { "$search": query.text(), "$language": "none" },
            })
            .projection(doc! { "_id": 1, "t": 1, "s": 1, "l": 1, "c": 1 })
            .sort(doc! { "score": { "$meta": "textScore" }, "_id": 1 })
            .limit(i64::try_from(query.limit()).map_err(|_| IssueStoreError::InvalidData)?)
            .await
            .map_err(|_| IssueStoreError::Unavailable)?;
        let mut results = Vec::with_capacity(query.limit());
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| IssueStoreError::Unavailable)?
        {
            results.push(decode_search_result(&document)?);
        }
        Ok(results)
    }

    async fn insert_regression_activity(&self, occurrence: &IssueOccurrence) {
        let document = doc! {
            "_id": binary(regression_activity_id(
                occurrence.project_id,
                occurrence.issue_id,
                occurrence.event_id,
            ).as_bytes()),
            "p": occurrence.project_id.get(),
            "u": binary(occurrence.issue_id.as_bytes()),
            "k": 6_i32,
            "a": actor_binary(ActorRef::system()),
            "e": binary(occurrence.event_id.as_bytes()),
            "t": date(occurrence.received_at),
        };
        insert_activity_best_effort(&self.database, document).await;
    }

    async fn insert_command_activity(&self, command: IssueCommand) {
        let mut document = doc! {
            "_id": binary(command_activity_id(command).as_bytes()),
            "p": command.project_id.get(),
            "u": binary(command.issue_id.as_bytes()),
            "k": i32::from(command.action.code()),
            "a": actor_binary(command.actor),
            "t": date(command.at),
        };
        if let IssueCommandAction::Assign(Some(assignee)) = command.action {
            document.insert("x", actor_binary(assignee));
        }
        insert_activity_best_effort(&self.database, document).await;
    }
}

impl IssueStore for MongoIssueStore {
    fn apply_occurrence(
        &self,
        occurrence: IssueOccurrence,
    ) -> PortFuture<'_, Result<IssueMutationResult, IssueStoreError>> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self.apply_occurrence_inner(occurrence).await;
            record_operation("issue_apply_occurrence", started, &result);
            result
        })
    }

    fn apply_command(
        &self,
        command: IssueCommand,
    ) -> PortFuture<'_, Result<IssueCommandResult, IssueStoreError>> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self.apply_command_inner(command).await;
            record_operation("issue_apply_command", started, &result);
            result
        })
    }

    fn load(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
    ) -> PortFuture<'_, Result<IssueSnapshot, IssueStoreError>> {
        Box::pin(self.load_inner(project_id, issue_id))
    }

    fn search_titles(
        &self,
        project_id: ProjectId,
        query: IssueSearchQuery,
    ) -> PortFuture<'_, Result<Vec<IssueSearchResult>, IssueStoreError>> {
        Box::pin(self.search_titles_inner(project_id, query))
    }
}

fn occurrence_pipeline(
    occurrence: &IssueOccurrence,
    increment: i64,
    body: Vec<u8>,
) -> Vec<Document> {
    let incoming_event = Bson::Binary(binary(occurrence.event_id.as_bytes()));
    let occurred = Bson::DateTime(date(occurrence.occurred_at));
    let received = Bson::DateTime(date(occurrence.received_at));
    let incoming_release = release_pair(occurrence.release.as_ref());
    let first = doc! { "$or": [
        { "$eq": [{ "$type": "$f" }, "missing"] },
        { "$lt": [occurred.clone(), "$f"] },
        { "$and": [
            { "$eq": [occurred.clone(), "$f"] },
            { "$lt": [incoming_event.clone(), "$e"] },
        ] },
    ] };
    let old_latest_event = doc! { "$ifNull": ["$v", "$e"] };
    let latest = doc! { "$or": [
        { "$eq": [{ "$type": "$l" }, "missing"] },
        { "$gt": [occurred.clone(), "$l"] },
        { "$and": [
            { "$eq": [occurred.clone(), "$l"] },
            { "$gt": [incoming_event.clone(), old_latest_event.clone()] },
        ] },
    ] };
    let old_first_release = doc! { "$cond": [
        { "$ne": [{ "$type": "$fr" }, "missing"] },
        { "p": true, "v": "$fr" },
        { "p": false, "v": "" },
    ] };
    let old_latest_release = doc! { "$cond": [
        { "$eq": ["$m", true] },
        { "p": false, "v": "" },
        { "$cond": [
            { "$ne": [{ "$type": "$lr" }, "missing"] },
            { "p": true, "v": "$lr" },
            old_first_release.clone(),
        ] },
    ] };
    let regression_count = doc! { "$cond": [
        { "$eq": [{ "$type": "$d" }, "missing"] },
        1_i64,
        { "$add": [{ "$ifNull": ["$d.c", 1_i64] }, 1_i64] },
    ] };
    let regression = doc! { "$cond": [
        { "$gt": [regression_count.clone(), 1_i64] },
        { "t": received.clone(), "e": incoming_event.clone(), "c": regression_count },
        { "t": received.clone(), "e": incoming_event.clone() },
    ] };
    vec![
        doc! { "$set": {
            "_fk_new": { "$eq": [{ "$type": "$b" }, "missing"] },
            "_fk_first": first,
            "_fk_latest": latest,
            "_fk_regress": { "$and": [
                { "$eq": ["$s", 1_i32] },
                { "$gt": [received, "$w.t"] },
            ] },
            "_fk_old_latest_event": old_latest_event,
            "_fk_old_first_release": old_first_release,
            "_fk_old_latest_release": old_latest_release,
        } },
        doc! { "$set": {
            "_fk_first_event": { "$cond": ["$_fk_first", incoming_event.clone(), "$e"] },
            "_fk_latest_event": { "$cond": ["$_fk_latest", incoming_event, "$_fk_old_latest_event"] },
            "_fk_first_release": { "$cond": ["$_fk_first", incoming_release.clone(), "$_fk_old_first_release"] },
            "_fk_latest_release": { "$cond": ["$_fk_latest", incoming_release, "$_fk_old_latest_release"] },
        } },
        doc! { "$set": {
            "p": { "$ifNull": ["$p", occurrence.project_id.get()] },
            "g": { "$ifNull": ["$g", binary(occurrence.grouping_key.to_bytes())] },
            "t": { "$cond": ["$_fk_new", occurrence.title.as_str(), "$t"] },
            "q": { "$cond": [
                "$_fk_new",
                occurrence.culprit.as_ref().map_or(Bson::String("$$REMOVE".to_owned()), |value| Bson::String(value.as_str().to_owned())),
                "$q",
            ] },
            "f": { "$cond": ["$_fk_first", occurred.clone(), "$f"] },
            "l": { "$cond": ["$_fk_latest", occurred, "$l"] },
            "e": "$_fk_first_event",
            "v": { "$cond": [
                { "$eq": ["$_fk_latest_event", "$_fk_first_event"] },
                "$$REMOVE",
                "$_fk_latest_event",
            ] },
            "c": { "$add": [{ "$ifNull": ["$c", 0_i64] }, increment] },
            "s": { "$cond": ["$_fk_regress", "$$REMOVE", "$s"] },
            "w": { "$cond": ["$_fk_regress", "$$REMOVE", "$w"] },
            "d": { "$cond": ["$_fk_regress", regression, "$d"] },
            "fr": { "$cond": ["$_fk_first_release.p", "$_fk_first_release.v", "$$REMOVE"] },
            "lr": { "$cond": [
                { "$and": [
                    "$_fk_latest_release.p",
                    { "$or": [
                        { "$not": ["$_fk_first_release.p"] },
                        { "$ne": ["$_fk_latest_release.v", "$_fk_first_release.v"] },
                    ] },
                ] },
                "$_fk_latest_release.v",
                "$$REMOVE",
            ] },
            "m": { "$cond": [
                { "$and": ["$_fk_first_release.p", { "$not": ["$_fk_latest_release.p"] }] },
                true,
                "$$REMOVE",
            ] },
            "b": { "$cond": ["$_fk_new", Binary {
                subtype: BinarySubtype::Generic,
                bytes: body,
            }, "$b"] },
        } },
        doc! { "$unset": [
            "_fk_new",
            "_fk_first",
            "_fk_latest",
            "_fk_regress",
            "_fk_old_latest_event",
            "_fk_old_first_release",
            "_fk_old_latest_release",
            "_fk_first_event",
            "_fk_latest_event",
            "_fk_first_release",
            "_fk_latest_release",
        ] },
    ]
}

fn encode_grouping_body(
    grouping: &IssueGroupingDetail,
    config: IssueCodecConfig,
) -> Result<Vec<u8>, IssueCodecError> {
    let components = grouping
        .explanation
        .components
        .iter()
        .map(|component| json!({ "k": component_kind_code(component.kind), "v": component.value }))
        .collect::<Vec<_>>();
    let value = json!({
        "s": strategy_code(grouping.strategy),
        "x": grouping.explanation.summary,
        "c": components,
    });
    let canonical = serde_json::to_vec(&value).map_err(|_| IssueCodecError::InvalidBody)?;
    if canonical.len() > config.max_decoded_body_bytes {
        return Err(IssueCodecError::TooLarge);
    }
    let compressed = zstd::bulk::compress(&canonical, config.compression_level)
        .map_err(|_| IssueCodecError::InvalidBody)?;
    let use_compressed = compressed
        .len()
        .saturating_add(config.compression_min_savings)
        <= canonical.len();
    let (codec, payload) = if use_compressed {
        (BODY_CODEC_ZSTD, compressed.as_slice())
    } else {
        (BODY_CODEC_JSON, canonical.as_slice())
    };
    let mut body = Vec::with_capacity(payload.len() + 2);
    body.extend_from_slice(&[BODY_FORMAT_VERSION, codec]);
    body.extend_from_slice(payload);
    Ok(body)
}

fn decode_grouping_body(
    bytes: &[u8],
    config: IssueCodecConfig,
) -> Result<IssueGroupingDetail, IssueCodecError> {
    let (&version, bytes) = bytes.split_first().ok_or(IssueCodecError::InvalidBody)?;
    let (&codec, payload) = bytes.split_first().ok_or(IssueCodecError::InvalidBody)?;
    if version != BODY_FORMAT_VERSION {
        return Err(IssueCodecError::InvalidBody);
    }
    let decoded = match codec {
        BODY_CODEC_JSON if payload.len() <= config.max_decoded_body_bytes => payload.to_vec(),
        BODY_CODEC_JSON => return Err(IssueCodecError::TooLarge),
        BODY_CODEC_ZSTD => zstd::bulk::decompress(payload, config.max_decoded_body_bytes)
            .map_err(|_| IssueCodecError::InvalidBody)?,
        _ => return Err(IssueCodecError::InvalidBody),
    };
    let value: Value =
        serde_json::from_slice(&decoded).map_err(|_| IssueCodecError::InvalidBody)?;
    if serde_json::to_vec(&value).map_err(|_| IssueCodecError::InvalidBody)? != decoded {
        return Err(IssueCodecError::InvalidBody);
    }
    let object = value.as_object().ok_or(IssueCodecError::InvalidBody)?;
    if object.len() != 3 {
        return Err(IssueCodecError::InvalidBody);
    }
    let strategy = parse_strategy(
        object
            .get("s")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .ok_or(IssueCodecError::InvalidBody)?,
    )?;
    let summary = object
        .get("x")
        .and_then(Value::as_str)
        .ok_or(IssueCodecError::InvalidBody)?
        .to_owned()
        .into_boxed_str();
    let components = object
        .get("c")
        .and_then(Value::as_array)
        .ok_or(IssueCodecError::InvalidBody)?
        .iter()
        .map(|value| {
            let object = value.as_object().ok_or(IssueCodecError::InvalidBody)?;
            if object.len() != 2 {
                return Err(IssueCodecError::InvalidBody);
            }
            Ok(GroupingComponent {
                kind: parse_component_kind(
                    object
                        .get("k")
                        .and_then(Value::as_u64)
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or(IssueCodecError::InvalidBody)?,
                )?,
                value: object
                    .get("v")
                    .and_then(Value::as_str)
                    .ok_or(IssueCodecError::InvalidBody)?
                    .to_owned()
                    .into_boxed_str(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(IssueGroupingDetail {
        strategy,
        explanation: GroupingExplanation {
            summary,
            components,
        },
    })
}

pub fn decode_issue(
    document: &Document,
    config: IssueCodecConfig,
) -> Result<IssueSnapshot, IssueCodecError> {
    let issue_id = IssueId::from_bytes(fixed_binary::<16>(document, "_id")?);
    let project_id = ProjectId::new(
        document
            .get_i32("p")
            .map_err(|_| IssueCodecError::InvalidDocument)?,
    )
    .map_err(|_| IssueCodecError::InvalidDocument)?;
    let grouping_key = GroupingKey::parse(&fixed_binary::<34>(document, "g")?)
        .map_err(|_| IssueCodecError::InvalidDocument)?;
    if !verify_issue_id(project_id, grouping_key, issue_id) {
        return Err(IssueCodecError::InvalidDocument);
    }
    let first_event_id = EventId::from_bytes(fixed_binary::<16>(document, "e")?);
    let latest_event_id = optional_fixed_binary::<16>(document, "v")?
        .map(EventId::from_bytes)
        .unwrap_or(first_event_id);
    let representative_event_id = optional_fixed_binary::<16>(document, "r")?
        .map(EventId::from_bytes)
        .unwrap_or(latest_event_id);
    let first_release = optional_release(document, "fr")?;
    let latest_release_missing = match document.get("m") {
        None => false,
        Some(Bson::Boolean(true)) => true,
        Some(_) => return Err(IssueCodecError::InvalidDocument),
    };
    let last_release = if latest_release_missing {
        if first_release.is_none() || document.contains_key("lr") {
            return Err(IssueCodecError::InvalidDocument);
        }
        None
    } else {
        optional_release(document, "lr")?.or_else(|| first_release.clone())
    };
    let status = decode_status(document)?;
    let workflow = match status {
        IssueStatus::Open if document.contains_key("w") => {
            return Err(IssueCodecError::InvalidDocument);
        }
        IssueStatus::Open => None,
        _ => Some(decode_workflow(
            document
                .get_document("w")
                .map_err(|_| IssueCodecError::InvalidDocument)?,
        )?),
    };
    let regression = match document.get("d") {
        None => None,
        Some(Bson::Document(value)) => Some(decode_regression(value)?),
        Some(_) => return Err(IssueCodecError::InvalidDocument),
    };
    let count = u64::try_from(
        document
            .get_i64("c")
            .map_err(|_| IssueCodecError::InvalidDocument)?,
    )
    .ok()
    .and_then(NonZeroU64::new)
    .ok_or(IssueCodecError::InvalidDocument)?;
    let body = document
        .get_binary_generic("b")
        .map_err(|_| IssueCodecError::InvalidDocument)?;
    Ok(IssueSnapshot {
        project_id,
        issue_id,
        grouping_key,
        title: IssueTitle::new(
            document
                .get_str("t")
                .map_err(|_| IssueCodecError::InvalidDocument)?,
        )
        .map_err(|_| IssueCodecError::InvalidDocument)?,
        culprit: optional_string(document, "q")?
            .map(IssueCulprit::new)
            .transpose()
            .map_err(|_| IssueCodecError::InvalidDocument)?,
        first_seen: decode_date(document, "f")?,
        last_seen: decode_date(document, "l")?,
        first_event_id,
        latest_event_id,
        representative_event_id,
        occurrence_count: count,
        status,
        assignee: optional_actor(document, "a")?,
        workflow,
        regression,
        first_release,
        last_release,
        grouping: decode_grouping_body(body, config)?,
    })
}

fn decode_search_result(document: &Document) -> Result<IssueSearchResult, IssueStoreError> {
    let issue_id = IssueId::from_bytes(
        fixed_binary::<16>(document, "_id").map_err(|_| IssueStoreError::InvalidData)?,
    );
    let count = u64::try_from(
        document
            .get_i64("c")
            .map_err(|_| IssueStoreError::InvalidData)?,
    )
    .ok()
    .and_then(NonZeroU64::new)
    .ok_or(IssueStoreError::InvalidData)?;
    Ok(IssueSearchResult {
        issue_id,
        title: IssueTitle::new(
            document
                .get_str("t")
                .map_err(|_| IssueStoreError::InvalidData)?,
        )
        .map_err(|_| IssueStoreError::InvalidData)?,
        status: decode_status(document).map_err(|_| IssueStoreError::InvalidData)?,
        last_seen: decode_date(document, "l").map_err(|_| IssueStoreError::InvalidData)?,
        occurrence_count: count,
    })
}

fn decode_status(document: &Document) -> Result<IssueStatus, IssueCodecError> {
    match document.get("s") {
        None => Ok(IssueStatus::Open),
        Some(Bson::Int32(1)) => Ok(IssueStatus::Resolved),
        Some(Bson::Int32(2)) => Ok(IssueStatus::Ignored),
        _ => Err(IssueCodecError::InvalidDocument),
    }
}

fn decode_workflow(document: &Document) -> Result<IssueWorkflow, IssueCodecError> {
    Ok(IssueWorkflow {
        at: decode_date(document, "t")?,
        actor: decode_actor(
            document
                .get_binary_generic("a")
                .map_err(|_| IssueCodecError::InvalidDocument)?,
        )?,
    })
}

fn decode_regression(document: &Document) -> Result<RegressionSummary, IssueCodecError> {
    let count = match document.get("c") {
        None => NonZeroU64::MIN,
        Some(Bson::Int64(value)) => u64::try_from(*value)
            .ok()
            .and_then(NonZeroU64::new)
            .filter(|value| value.get() > 1)
            .ok_or(IssueCodecError::InvalidDocument)?,
        _ => return Err(IssueCodecError::InvalidDocument),
    };
    Ok(RegressionSummary {
        at: decode_date(document, "t")?,
        event_id: EventId::from_bytes(fixed_binary::<16>(document, "e")?),
        count,
    })
}

fn decode_date(document: &Document, name: &str) -> Result<Timestamp, IssueCodecError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(name)
            .map_err(|_| IssueCodecError::InvalidDocument)?
            .timestamp_millis(),
    )
    .map_err(|_| IssueCodecError::InvalidDocument)
}

fn optional_actor(document: &Document, name: &str) -> Result<Option<ActorRef>, IssueCodecError> {
    match document.get(name) {
        None => Ok(None),
        Some(Bson::Binary(value)) if value.subtype == BinarySubtype::Generic => {
            decode_actor(&value.bytes).map(Some)
        }
        Some(_) => Err(IssueCodecError::InvalidDocument),
    }
}

fn decode_actor(bytes: &[u8]) -> Result<ActorRef, IssueCodecError> {
    ActorRef::from_bytes(
        bytes
            .try_into()
            .map_err(|_| IssueCodecError::InvalidDocument)?,
    )
    .ok_or(IssueCodecError::InvalidDocument)
}

fn optional_release(
    document: &Document,
    name: &str,
) -> Result<Option<IssueRelease>, IssueCodecError> {
    optional_string(document, name)?
        .map(IssueRelease::new)
        .transpose()
        .map_err(|_| IssueCodecError::InvalidDocument)
}

fn optional_string<'a>(
    document: &'a Document,
    name: &str,
) -> Result<Option<&'a str>, IssueCodecError> {
    match document.get(name) {
        None => Ok(None),
        Some(Bson::String(value)) => Ok(Some(value)),
        Some(_) => Err(IssueCodecError::InvalidDocument),
    }
}

fn fixed_binary<const N: usize>(
    document: &Document,
    name: &str,
) -> Result<[u8; N], IssueCodecError> {
    document
        .get_binary_generic(name)
        .map_err(|_| IssueCodecError::InvalidDocument)?
        .as_slice()
        .try_into()
        .map_err(|_| IssueCodecError::InvalidDocument)
}

fn optional_fixed_binary<const N: usize>(
    document: &Document,
    name: &str,
) -> Result<Option<[u8; N]>, IssueCodecError> {
    match document.get(name) {
        None => Ok(None),
        Some(Bson::Binary(value)) if value.subtype == BinarySubtype::Generic => value
            .bytes
            .as_slice()
            .try_into()
            .map(Some)
            .map_err(|_| IssueCodecError::InvalidDocument),
        Some(_) => Err(IssueCodecError::InvalidDocument),
    }
}

fn strategy_code(strategy: GroupingStrategy) -> u8 {
    match strategy {
        GroupingStrategy::SdkFingerprint => 1,
        GroupingStrategy::ExceptionStack => 2,
        GroupingStrategy::NativeStack => 3,
        GroupingStrategy::Message => 4,
    }
}

fn parse_strategy(code: u8) -> Result<GroupingStrategy, IssueCodecError> {
    match code {
        1 => Ok(GroupingStrategy::SdkFingerprint),
        2 => Ok(GroupingStrategy::ExceptionStack),
        3 => Ok(GroupingStrategy::NativeStack),
        4 => Ok(GroupingStrategy::Message),
        _ => Err(IssueCodecError::InvalidBody),
    }
}

fn component_kind_code(kind: GroupingComponentKind) -> u8 {
    kind as u8
}

fn parse_component_kind(code: u8) -> Result<GroupingComponentKind, IssueCodecError> {
    match code {
        1 => Ok(GroupingComponentKind::SdkFingerprint),
        2 => Ok(GroupingComponentKind::DefaultStrategy),
        3 => Ok(GroupingComponentKind::DefaultDigest),
        4 => Ok(GroupingComponentKind::ExceptionType),
        5 => Ok(GroupingComponentKind::Frame),
        6 => Ok(GroupingComponentKind::FrameFunction),
        7 => Ok(GroupingComponentKind::FrameModule),
        8 => Ok(GroupingComponentKind::FramePath),
        9 => Ok(GroupingComponentKind::FrameLine),
        10 => Ok(GroupingComponentKind::NativeModule),
        11 => Ok(GroupingComponentKind::NativeRelativeAddress),
        12 => Ok(GroupingComponentKind::Logger),
        13 => Ok(GroupingComponentKind::Message),
        _ => Err(IssueCodecError::InvalidBody),
    }
}

fn release_pair(release: Option<&IssueRelease>) -> Document {
    release.map_or_else(
        || doc! { "p": false, "v": "" },
        |release| doc! { "p": true, "v": release.as_str() },
    )
}

fn and_filter(base: Document, condition: Document) -> Document {
    doc! { "$and": [base, condition] }
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn binary<const N: usize>(bytes: [u8; N]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn actor_binary(actor: ActorRef) -> Binary {
    binary(actor.to_bytes())
}

fn duplicate_write(error: &MongoError) -> bool {
    matches!(
        error.kind.as_ref(),
        ErrorKind::Write(WriteFailure::WriteError(write)) if write.code == DUPLICATE_KEY_CODE
    ) || matches!(
        error.kind.as_ref(),
        ErrorKind::Command(command) if command.code == DUPLICATE_KEY_CODE
    )
}

async fn insert_activity_best_effort(database: &Database, document: Document) {
    let result = database
        .collection::<Document>("issue_activities")
        .insert_one(document)
        .await;
    if let Err(error) = result
        && !duplicate_write(&error)
    {
        metrics::counter!(
            "faultkeep_mongodb_operation_errors_total",
            "operation" => "issue_activity_insert"
        )
        .increment(1);
    }
}

fn record_operation<T>(
    operation: &'static str,
    started: Instant,
    result: &Result<T, IssueStoreError>,
) {
    let outcome = match result {
        Ok(_) => "ok",
        Err(IssueStoreError::IdentityCollision) => "collision",
        Err(IssueStoreError::NotFound) => "not_found",
        Err(IssueStoreError::InvalidData) => "invalid_data",
        Err(IssueStoreError::Unavailable) => "unavailable",
    };
    metrics::histogram!(
        "faultkeep_mongodb_operation_duration_seconds",
        "operation" => operation,
        "outcome" => outcome
    )
    .record(started.elapsed().as_secs_f64());
}

pub(crate) fn issue_validator() -> Document {
    doc! { "$and": [
        { "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "p", "g", "t", "f", "l", "e", "c", "b"],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int", "minimum": 1 },
                "g": { "bsonType": "binData" },
                "t": { "bsonType": "string", "minLength": 1 },
                "q": { "bsonType": "string", "minLength": 1 },
                "f": { "bsonType": "date" },
                "l": { "bsonType": "date" },
                "e": { "bsonType": "binData" },
                "v": { "bsonType": "binData" },
                "r": { "bsonType": "binData" },
                "c": { "bsonType": "long", "minimum": 1 },
                "s": { "bsonType": "int", "enum": [1, 2] },
                "a": { "bsonType": "binData" },
                "w": {
                    "bsonType": "object",
                    "required": ["t", "a"],
                    "additionalProperties": false,
                    "properties": {
                        "t": { "bsonType": "date" },
                        "a": { "bsonType": "binData" },
                    },
                },
                "d": {
                    "bsonType": "object",
                    "required": ["t", "e"],
                    "additionalProperties": false,
                    "properties": {
                        "t": { "bsonType": "date" },
                        "e": { "bsonType": "binData" },
                        "c": { "bsonType": "long", "minimum": 2 },
                    },
                },
                "fr": { "bsonType": "string", "minLength": 1 },
                "lr": { "bsonType": "string", "minLength": 1 },
                "m": { "enum": [true] },
                "j": { "enum": [true] },
                "n": { "bsonType": "array", "maxItems": 64 },
                "b": { "bsonType": "binData" },
            },
        } },
        { "$expr": { "$and": [
            { "$eq": [{ "$binarySize": "$_id" }, 16] },
            { "$eq": [{ "$binarySize": "$g" }, 34] },
            { "$eq": [{ "$binarySize": "$e" }, 16] },
            { "$lte": [{ "$strLenBytes": "$t" }, 512] },
            { "$cond": [
                { "$ne": [{ "$type": "$q" }, "missing"] },
                { "$lte": [{ "$strLenBytes": "$q" }, 256] },
                true,
            ] },
            { "$cond": [
                { "$ne": [{ "$type": "$v" }, "missing"] },
                { "$eq": [{ "$binarySize": "$v" }, 16] },
                true,
            ] },
            { "$cond": [
                { "$ne": [{ "$type": "$r" }, "missing"] },
                { "$eq": [{ "$binarySize": "$r" }, 16] },
                true,
            ] },
            { "$cond": [
                { "$ne": [{ "$type": "$a" }, "missing"] },
                { "$eq": [{ "$binarySize": "$a" }, 17] },
                true,
            ] },
            { "$cond": [
                { "$ne": [{ "$type": "$w" }, "missing"] },
                { "$eq": [{ "$binarySize": "$w.a" }, 17] },
                true,
            ] },
            { "$not": [{ "$and": [
                { "$ne": [{ "$type": "$lr" }, "missing"] },
                { "$eq": ["$m", true] },
            ] }] },
            { "$cond": [
                { "$eq": ["$m", true] },
                { "$ne": [{ "$type": "$fr" }, "missing"] },
                true,
            ] },
        ] } },
    ] }
}

pub(crate) fn issue_activity_validator() -> Document {
    doc! { "$and": [
        { "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "p", "u", "k", "a", "t"],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int", "minimum": 1 },
                "u": { "bsonType": "binData" },
                "k": { "bsonType": "int", "minimum": 1, "maximum": 6 },
                "a": { "bsonType": "binData" },
                "x": { "bsonType": "binData" },
                "e": { "bsonType": "binData" },
                "t": { "bsonType": "date" },
            },
        } },
        { "$expr": { "$and": [
            { "$eq": [{ "$binarySize": "$_id" }, 16] },
            { "$eq": [{ "$binarySize": "$u" }, 16] },
            { "$eq": [{ "$binarySize": "$a" }, 17] },
            { "$cond": [
                { "$ne": [{ "$type": "$x" }, "missing"] },
                { "$eq": [{ "$binarySize": "$x" }, 17] },
                true,
            ] },
            { "$cond": [
                { "$ne": [{ "$type": "$e" }, "missing"] },
                { "$eq": [{ "$binarySize": "$e" }, 16] },
                true,
            ] },
        ] } },
    ] }
}

pub(crate) fn issue_index_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "_id_",
        "issue_notification_ready",
        "issue_project_timeline",
        "issue_status_timeline",
        "issue_title_text",
    ])
}

pub(crate) fn issue_activity_index_names() -> BTreeSet<&'static str> {
    BTreeSet::from(["_id_", "issue_activity_timeline"])
}

pub(crate) async fn create_issue_indexes(database: &Database) -> Result<(), MongoError> {
    let issues = database.collection::<Document>("issues");
    for index in issue_indexes() {
        issues.create_index(index).await?;
    }
    database
        .collection::<Document>("issue_activities")
        .create_index(named_index(
            doc! { "p": 1, "u": 1, "t": -1, "_id": -1 },
            "issue_activity_timeline",
            None,
        ))
        .await?;
    Ok(())
}

pub(crate) async fn validate_issue_indexes(database: &Database) -> Result<bool, MongoError> {
    let issues_valid =
        validate_indexes(database, "issues", issue_indexes().into_iter().collect()).await?;
    let activities_valid = validate_indexes(
        database,
        "issue_activities",
        vec![named_index(
            doc! { "p": 1, "u": 1, "t": -1, "_id": -1 },
            "issue_activity_timeline",
            None,
        )],
    )
    .await?;
    Ok(issues_valid && activities_valid)
}

fn issue_indexes() -> [IndexModel; 4] {
    [
        named_index(
            doc! { "p": 1, "l": -1, "_id": -1 },
            "issue_project_timeline",
            None,
        ),
        named_index(
            doc! { "p": 1, "s": 1, "l": -1, "_id": -1 },
            "issue_status_timeline",
            None,
        ),
        named_index(
            doc! { "j": 1, "_id": 1 },
            "issue_notification_ready",
            Some(doc! { "j": true }),
        ),
        IndexModel::builder()
            .keys(doc! { "p": 1, "t": "text" })
            .options(
                IndexOptions::builder()
                    .name("issue_title_text".to_owned())
                    .default_language("none".to_owned())
                    .weights(doc! { "t": 1 })
                    .collation(Collation::builder().locale("simple".to_owned()).build())
                    .build(),
            )
            .build(),
    ]
}

fn named_index(keys: Document, name: &str, partial: Option<Document>) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .partial_filter_expression(partial)
                .build(),
        )
        .build()
}

async fn validate_indexes(
    database: &Database,
    collection: &str,
    models: Vec<IndexModel>,
) -> Result<bool, MongoError> {
    let mut expected = models
        .into_iter()
        .map(|model| {
            let name = model
                .options
                .as_ref()
                .and_then(|options| options.name.clone())
                .expect("owned Issue index has a name");
            (name, model)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut indexes = database
        .collection::<Document>(collection)
        .list_indexes()
        .await?;
    while let Some(actual) = indexes.try_next().await? {
        let Some(options) = actual.options.as_ref() else {
            return Ok(false);
        };
        let Some(name) = options.name.as_deref() else {
            return Ok(false);
        };
        if name == "_id_" {
            continue;
        }
        let Some(expected_model) = expected.remove(name) else {
            return Ok(false);
        };
        let expected_options = expected_model
            .options
            .as_ref()
            .expect("Issue index options");
        let keys_compatible = if name == "issue_title_text" {
            actual.keys.get_i32("p") == Ok(1)
                && actual.keys.get_str("_fts") == Ok("text")
                && actual.keys.get_i32("_ftsx") == Ok(1)
        } else {
            actual.keys == expected_model.keys
        };
        let collation_compatible = options
            .collation
            .as_ref()
            .map(|value| value.locale.as_str())
            == expected_options
                .collation
                .as_ref()
                .map(|value| value.locale.as_str())
            || (options.collation.is_none()
                && expected_options
                    .collation
                    .as_ref()
                    .is_some_and(|value| value.locale == "simple"));
        let compatible = keys_compatible
            && options.partial_filter_expression == expected_options.partial_filter_expression
            && options.default_language == expected_options.default_language
            && options.weights == expected_options.weights
            && collation_compatible;
        if !compatible {
            return Ok(false);
        }
    }
    Ok(expected.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use faultkeep_domain::grouping::{GroupingExplanation, derive_issue_id};

    fn occurrence(release: Option<&str>) -> IssueOccurrence {
        let project_id = ProjectId::new(7).unwrap();
        let key = GroupingKey::parse(&[&1_u16.to_be_bytes()[..], &[3; 32][..]].concat()).unwrap();
        IssueOccurrence {
            project_id,
            issue_id: derive_issue_id(project_id, key),
            grouping_key: key,
            event_id: EventId::from_bytes([4; 16]),
            occurred_at: Timestamp::from_unix_millis(1_000).unwrap(),
            received_at: Timestamp::from_unix_millis(2_000).unwrap(),
            release: release.map(|value| IssueRelease::new(value).unwrap()),
            title: IssueTitle::new("Panic: bounded failure").unwrap(),
            culprit: Some(IssueCulprit::new("crate::serve").unwrap()),
            grouping: IssueGroupingDetail {
                strategy: GroupingStrategy::Message,
                explanation: GroupingExplanation {
                    summary: "logger plus normalized message".into(),
                    components: vec![GroupingComponent {
                        kind: GroupingComponentKind::Message,
                        value: "bounded failure".into(),
                    }],
                },
            },
            increment: NonZeroU64::MIN,
        }
    }

    fn initial_document(occurrence: &IssueOccurrence, config: IssueCodecConfig) -> Document {
        let mut document = doc! {
            "_id": binary(occurrence.issue_id.as_bytes()),
            "p": occurrence.project_id.get(),
            "g": binary(occurrence.grouping_key.to_bytes()),
            "t": occurrence.title.as_str(),
            "q": occurrence.culprit.as_ref().unwrap().as_str(),
            "f": date(occurrence.occurred_at),
            "l": date(occurrence.occurred_at),
            "e": binary(occurrence.event_id.as_bytes()),
            "c": 1_i64,
            "b": Binary {
                subtype: BinarySubtype::Generic,
                bytes: encode_grouping_body(&occurrence.grouping, config).unwrap(),
            },
        };
        if let Some(release) = &occurrence.release {
            document.insert("fr", release.as_str());
        }
        document
    }

    #[test]
    fn compact_issue_codec_round_trips_and_has_golden_size() {
        let config = IssueCodecConfig::default();
        let occurrence = occurrence(Some("1.0.0"));
        let document = initial_document(&occurrence, config);
        let decoded = decode_issue(&document, config).unwrap();
        assert_eq!(decoded.issue_id, occurrence.issue_id);
        assert_eq!(decoded.first_release, occurrence.release);
        assert_eq!(decoded.last_release, occurrence.release);
        assert_eq!(decoded.grouping, occurrence.grouping);
        assert_eq!(mongodb::bson::to_vec(&document).unwrap().len(), 292);
    }

    #[test]
    fn codec_distinguishes_missing_latest_release_and_rejects_malformed_body() {
        let config = IssueCodecConfig::default();
        let occurrence = occurrence(Some("1.0.0"));
        let mut document = initial_document(&occurrence, config);
        document.insert("m", true);
        assert_eq!(decode_issue(&document, config).unwrap().last_release, None);
        document.insert("lr", "2.0.0");
        assert_eq!(
            decode_issue(&document, config),
            Err(IssueCodecError::InvalidDocument)
        );
        document.remove("lr");
        document.insert("b", binary([99, 0]));
        assert_eq!(
            decode_issue(&document, config),
            Err(IssueCodecError::InvalidBody)
        );
        document = initial_document(&occurrence, config);
        document.insert("q", Bson::Null);
        assert_eq!(
            decode_issue(&document, config),
            Err(IssueCodecError::InvalidDocument)
        );
    }

    #[test]
    fn deterministic_codec_property_corpus_preserves_optional_defaults() {
        let config = IssueCodecConfig::default();
        for seed in 1_u8..=64 {
            let mut value = occurrence((seed % 2 == 0).then_some("first"));
            let mut key = [seed; 34];
            key[..2].copy_from_slice(&1_u16.to_be_bytes());
            value.grouping_key = GroupingKey::parse(&key).unwrap();
            value.issue_id = derive_issue_id(value.project_id, value.grouping_key);
            value.event_id = EventId::from_bytes([seed; 16]);
            value.title = IssueTitle::new(format!("Ошибка {seed}: deterministic")).unwrap();
            let mut document = initial_document(&value, config);
            if seed % 3 == 0 {
                document.insert("v", binary([seed.wrapping_add(1); 16]));
            }
            if seed % 5 == 0 {
                document.insert("r", binary([seed.wrapping_add(2); 16]));
            }
            let decoded = decode_issue(&document, config).unwrap();
            assert_eq!(decoded.issue_id, value.issue_id);
            assert_eq!(decoded.grouping_key, value.grouping_key);
            assert_eq!(decoded.title, value.title);
            assert_eq!(decoded.first_release, value.release);
            assert_eq!(
                decoded.latest_event_id,
                if seed % 3 == 0 {
                    EventId::from_bytes([seed.wrapping_add(1); 16])
                } else {
                    value.event_id
                }
            );
        }
    }

    #[test]
    fn grouping_body_adaptively_compresses_and_round_trips_components() {
        let config = IssueCodecConfig::default();
        let mut grouping = occurrence(None).grouping;
        grouping.explanation.components = (0..64)
            .map(|_| GroupingComponent {
                kind: GroupingComponentKind::Message,
                value: "repeated bounded component".into(),
            })
            .collect();
        let encoded = encode_grouping_body(&grouping, config).unwrap();
        assert_eq!(&encoded[..2], &[BODY_FORMAT_VERSION, BODY_CODEC_ZSTD]);
        assert_eq!(decode_grouping_body(&encoded, config).unwrap(), grouping);
    }
}
