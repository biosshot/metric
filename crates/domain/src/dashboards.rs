//! Shared, project-scoped saved Explore queries and bounded dashboard configuration.

use std::fmt;

use thiserror::Error;

use crate::{
    ProjectId, Timestamp,
    auth::UserId,
    explore::{ExploreQuery, ExploreResult},
};

pub const MAX_SAVED_QUERY_NAME_BYTES: usize = 120;
pub const MAX_DASHBOARD_NAME_BYTES: usize = 120;
pub const MAX_WIDGET_TITLE_BYTES: usize = 120;
pub const MAX_DASHBOARD_WIDGETS: usize = 8;

macro_rules! binary_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 16]);

        impl $name {
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            #[must_use]
            pub const fn as_bytes(self) -> [u8; 16] {
                self.0
            }

            pub fn parse(value: &str) -> Result<Self, DashboardValueError> {
                let bytes = hex::decode(value).map_err(|_| DashboardValueError)?;
                let bytes: [u8; 16] = bytes.try_into().map_err(|_| DashboardValueError)?;
                Ok(Self(bytes))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&hex::encode(self.0))
            }
        }
    };
}

binary_id!(SavedQueryId);
binary_id!(DashboardId);
binary_id!(DashboardWidgetId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("dashboard value is invalid")]
pub struct DashboardValueError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetShape {
    Number,
    Table,
    Timeseries,
}

impl WidgetShape {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::Table => "table",
            Self::Timeseries => "timeseries",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DashboardValueError> {
        match value {
            "number" => Ok(Self::Number),
            "table" => Ok(Self::Table),
            "timeseries" => Ok(Self::Timeseries),
            _ => Err(DashboardValueError),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardRefreshInterval {
    Manual,
    ThirtySeconds,
    OneMinute,
    FiveMinutes,
}

impl DashboardRefreshInterval {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::ThirtySeconds => "30s",
            Self::OneMinute => "1m",
            Self::FiveMinutes => "5m",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DashboardValueError> {
        match value {
            "manual" => Ok(Self::Manual),
            "30s" => Ok(Self::ThirtySeconds),
            "1m" => Ok(Self::OneMinute),
            "5m" => Ok(Self::FiveMinutes),
            _ => Err(DashboardValueError),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavedQuery {
    pub id: SavedQueryId,
    pub project_id: ProjectId,
    pub name: Box<str>,
    pub query: ExploreQuery,
    pub revision: u64,
    pub created_by: UserId,
    pub updated_by: UserId,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl SavedQuery {
    pub fn validate(&self) -> Result<(), DashboardValueError> {
        validate_text(&self.name, MAX_SAVED_QUERY_NAME_BYTES)?;
        if self.revision == 0 || self.query.cursor.is_some() || self.updated_at < self.created_at {
            return Err(DashboardValueError);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardWidget {
    pub id: DashboardWidgetId,
    pub title: Box<str>,
    pub saved_query_id: SavedQueryId,
    pub shape: WidgetShape,
}

impl DashboardWidget {
    pub fn validate(&self) -> Result<(), DashboardValueError> {
        validate_text(&self.title, MAX_WIDGET_TITLE_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dashboard {
    pub id: DashboardId,
    pub project_id: ProjectId,
    pub name: Box<str>,
    pub widgets: Vec<DashboardWidget>,
    pub refresh_interval: DashboardRefreshInterval,
    pub revision: u64,
    pub created_by: UserId,
    pub updated_by: UserId,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Dashboard {
    pub fn validate(&self) -> Result<(), DashboardValueError> {
        validate_text(&self.name, MAX_DASHBOARD_NAME_BYTES)?;
        if self.widgets.is_empty()
            || self.widgets.len() > MAX_DASHBOARD_WIDGETS
            || self.revision == 0
            || self.updated_at < self.created_at
        {
            return Err(DashboardValueError);
        }
        let mut ids = self
            .widgets
            .iter()
            .map(|widget| widget.id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(DashboardValueError);
        }
        self.widgets.iter().try_for_each(DashboardWidget::validate)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DashboardVariables {
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashboardWidgetResult {
    pub widget_id: DashboardWidgetId,
    pub cost: Option<u32>,
    pub result: Option<ExploreResult>,
    pub error_code: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashboardRefresh {
    pub dashboard_id: DashboardId,
    pub refreshed_at: Timestamp,
    pub total_cost: u32,
    pub widgets: Vec<DashboardWidgetResult>,
}

fn validate_text(value: &str, maximum: usize) -> Result<(), DashboardValueError> {
    if value.trim().is_empty()
        || value.len() > maximum
        || value.chars().any(|character| character == '\0')
    {
        return Err(DashboardValueError);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Timestamp,
        explore::{ExploreDataset, ExploreQuery},
    };

    fn query() -> ExploreQuery {
        ExploreQuery {
            dataset: ExploreDataset::Logs,
            from: Timestamp::from_unix_millis(1).unwrap(),
            until: Timestamp::from_unix_millis(2).unwrap(),
            predicates: Vec::new(),
            expression: None,
            aggregates: Vec::new(),
            group_by: Vec::new(),
            interval: None,
            cursor: None,
            limit: 50,
        }
    }

    #[test]
    fn configuration_is_bounded_and_shared_records_are_revisioned() {
        let now = Timestamp::from_unix_millis(1_000).unwrap();
        let saved = SavedQuery {
            id: SavedQueryId::from_bytes([1; 16]),
            project_id: ProjectId::new(7).unwrap(),
            name: "Recent logs".into(),
            query: query(),
            revision: 1,
            created_by: UserId::new(1).unwrap(),
            updated_by: UserId::new(1).unwrap(),
            created_at: now,
            updated_at: now,
        };
        assert!(saved.validate().is_ok());

        let widget = DashboardWidget {
            id: DashboardWidgetId::from_bytes([2; 16]),
            title: "Logs".into(),
            saved_query_id: saved.id,
            shape: WidgetShape::Table,
        };
        let mut dashboard = Dashboard {
            id: DashboardId::from_bytes([3; 16]),
            project_id: saved.project_id,
            name: "Operations".into(),
            widgets: vec![widget.clone()],
            refresh_interval: DashboardRefreshInterval::Manual,
            revision: 1,
            created_by: saved.created_by,
            updated_by: saved.updated_by,
            created_at: now,
            updated_at: now,
        };
        assert!(dashboard.validate().is_ok());
        dashboard.widgets = vec![widget; MAX_DASHBOARD_WIDGETS + 1];
        assert_eq!(dashboard.validate(), Err(DashboardValueError));
    }
}
