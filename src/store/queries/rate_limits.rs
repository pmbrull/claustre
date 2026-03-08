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
