//! Rate limit state operations.

use anyhow::Result;
use rusqlite::params;

use crate::store::Store;
use crate::store::models::RateLimitState;

impl Store {
    pub fn get_rate_limit_state(&self) -> Result<RateLimitState> {
        let state = self.conn.query_row(
            "SELECT is_rate_limited, limit_type, rate_limited_at, reset_at,
                    usage_5h_pct, usage_7d_pct, updated_at
             FROM rate_limit_state WHERE id = 1",
            [],
            |row| {
                let is_rate_limited: i64 = row.get(0)?;
                Ok(RateLimitState {
                    is_rate_limited: is_rate_limited != 0,
                    limit_type: row.get(1)?,
                    rate_limited_at: row.get(2)?,
                    reset_at: row.get(3)?,
                    usage_5h_pct: Some(row.get(4)?),
                    usage_7d_pct: Some(row.get(5)?),
                    reset_5h: None,
                    reset_7d: None,
                    updated_at: row.get(6)?,
                })
            },
        )?;
        Ok(state)
    }

    #[cfg(test)]
    #[expect(
        clippy::similar_names,
        reason = "5h and 7d are distinct domain-specific window labels"
    )]
    pub fn set_rate_limited(
        &self,
        limit_type: &str,
        reset_at: &str,
        usage_5h_pct: f64,
        usage_7d_pct: f64,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                is_rate_limited = 1,
                limit_type = ?1,
                rate_limited_at = ?2,
                reset_at = ?3,
                usage_5h_pct = ?4,
                usage_7d_pct = ?5,
                updated_at = ?2
             WHERE id = 1",
            params![limit_type, now, reset_at, usage_5h_pct, usage_7d_pct],
        )?;
        Ok(())
    }

    pub fn clear_rate_limit(&self) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                is_rate_limited = 0,
                limit_type = NULL,
                rate_limited_at = NULL,
                reset_at = NULL,
                updated_at = ?1
             WHERE id = 1",
            params![now],
        )?;
        Ok(())
    }

    #[cfg(test)]
    #[expect(
        clippy::similar_names,
        reason = "5h and 7d are distinct domain-specific window labels"
    )]
    pub fn update_usage_windows(&self, usage_5h_pct: f64, usage_7d_pct: f64) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                usage_5h_pct = ?1,
                usage_7d_pct = ?2,
                updated_at = ?3
             WHERE id = 1",
            params![usage_5h_pct, usage_7d_pct, now],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Store;

    #[test]
    fn default_rate_limit_state_is_not_limited() {
        let store = Store::open_in_memory().unwrap();
        let state = store.get_rate_limit_state().unwrap();
        assert!(!state.is_rate_limited);
        assert!(state.limit_type.is_none());
        assert!(state.reset_at.is_none());
    }

    #[test]
    fn set_and_get_rate_limited() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_rate_limited("standard", "2025-01-01T02:00:00Z", 85.0, 45.0)
            .unwrap();

        let state = store.get_rate_limit_state().unwrap();
        assert!(state.is_rate_limited);
        assert_eq!(state.limit_type.as_deref(), Some("standard"));
        assert_eq!(state.reset_at.as_deref(), Some("2025-01-01T02:00:00Z"));
        assert!((state.usage_5h_pct.unwrap() - 85.0).abs() < f64::EPSILON);
        assert!((state.usage_7d_pct.unwrap() - 45.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clear_rate_limit_resets_state() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_rate_limited("standard", "2025-01-01T02:00:00Z", 85.0, 45.0)
            .unwrap();

        store.clear_rate_limit().unwrap();

        let state = store.get_rate_limit_state().unwrap();
        assert!(!state.is_rate_limited);
        assert!(state.limit_type.is_none());
        assert!(state.reset_at.is_none());
    }

    #[test]
    fn update_usage_windows_changes_percentages() {
        let store = Store::open_in_memory().unwrap();
        store.update_usage_windows(50.0, 25.0).unwrap();

        let state = store.get_rate_limit_state().unwrap();
        assert!((state.usage_5h_pct.unwrap() - 50.0).abs() < f64::EPSILON);
        assert!((state.usage_7d_pct.unwrap() - 25.0).abs() < f64::EPSILON);
        // Should still not be rate-limited
        assert!(!state.is_rate_limited);
    }

    #[test]
    fn rate_limit_cycle_set_clear_set() {
        let store = Store::open_in_memory().unwrap();

        // Set rate limited
        store
            .set_rate_limited("overloaded", "2025-06-15T12:00:00Z", 100.0, 80.0)
            .unwrap();
        assert!(store.get_rate_limit_state().unwrap().is_rate_limited);

        // Clear
        store.clear_rate_limit().unwrap();
        assert!(!store.get_rate_limit_state().unwrap().is_rate_limited);

        // Set again with different type
        store
            .set_rate_limited("standard", "2025-06-15T14:00:00Z", 90.0, 70.0)
            .unwrap();
        let state = store.get_rate_limit_state().unwrap();
        assert!(state.is_rate_limited);
        assert_eq!(state.limit_type.as_deref(), Some("standard"));
    }
}
