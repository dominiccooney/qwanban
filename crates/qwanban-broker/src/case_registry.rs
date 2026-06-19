//! Case registry: tracks open cases + their case_tokens (the auth binding).
//! Pure logic; the HTTP/gRPC server is layered above this.

use qwanban_proto::id::CaseId;
use qwanban_proto::QwanCode;
use parking_lot::Mutex;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CaseRecord {
    pub case_id: CaseId,
    pub case_token: String,
    pub status: qwanban_proto::broker::CaseStatus,
}

pub struct CaseRegistry {
    cases: Mutex<HashMap<CaseId, CaseRecord>>,
}

impl CaseRegistry {
    pub fn new() -> Self {
        Self {
            cases: Mutex::new(HashMap::new()),
        }
    }

    pub fn open(&self, case_id: CaseId, case_token: String) {
        self.cases.lock().insert(
            case_id.clone(),
            CaseRecord {
                case_id,
                case_token,
                status: qwanban_proto::broker::CaseStatus::Booting,
            },
        );
    }

    /// Verify the case_token matches the case; returns InvalidArg/Unauthenticated.
    pub fn verify(&self, case_id: &CaseId, case_token: &str) -> qwanban_proto::QwanResult<()> {
        let guard = self.cases.lock();
        let rec = guard
            .get(case_id)
            .ok_or_else(|| qwanban_proto::not_found(format!("unknown case {case_id}")))?;
        if rec.case_token != case_token {
            return Err(qwanban_proto::QwanError::new(
                QwanCode::Unauthenticated,
                "bad case_token",
            ));
        }
        Ok(())
    }

    pub fn set_status(&self, case_id: &CaseId, status: qwanban_proto::broker::CaseStatus) {
        if let Some(r) = self.cases.lock().get_mut(case_id) {
            r.status = status;
        }
    }

    pub fn close(&self, case_id: &CaseId) {
        self.cases.lock().remove(case_id);
    }

    pub fn live_count(&self) -> u32 {
        self.cases.lock().len() as u32
    }
}

impl Default for CaseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_rejects_bad_token() {
        let r = CaseRegistry::new();
        let c = CaseId::from_str_inner("case_1");
        r.open(c.clone(), "tok".into());
        assert!(r.verify(&c, "tok").is_ok());
        let err = r.verify(&c, "wrong").unwrap_err();
        assert_eq!(err.code(), QwanCode::Unauthenticated);
    }

    #[test]
    fn verify_rejects_unknown_case() {
        let r = CaseRegistry::new();
        let c = CaseId::from_str_inner("nope");
        let err = r.verify(&c, "tok").unwrap_err();
        assert_eq!(err.code(), QwanCode::NotFound);
    }
}
