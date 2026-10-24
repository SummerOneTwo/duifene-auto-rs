use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Course {
    pub class_id: String,
    pub tclass_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignType {
    Code,
    Qr,
    Locate,
    /// 汉字签到(九宫格点选):升级新增,提交接口与数字码相同,码为 4 个汉字
    Hanzi,
    Unknown,
}

impl SignType {
    pub fn parse(s: &str) -> SignType {
        match s {
            "1" => SignType::Code,
            "2" => SignType::Qr,
            "3" => SignType::Locate,
            "4" => SignType::Hanzi,
            _ => SignType::Unknown,
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct RawRow {
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "CheckInType", default)]
    pub checkin_type: String,
    #[serde(rename = "CheckInCode", default)]
    pub checkin_code: String,
    #[serde(rename = "CheckInStatus", default)]
    pub checkin_status: String,
    #[serde(rename = "CheckInDate", default)]
    pub checkin_date: String,
    #[serde(rename = "StudentID", default)]
    pub student_id: String,
    #[serde(rename = "StatusID", default)]
    pub status_id: String,
    #[serde(rename = "StatusName", default)]
    pub status_name: String,
    #[serde(rename = "CanApply", default)]
    pub can_apply: String,
    #[serde(rename = "ApplyLimitDate", default)]
    pub apply_limit: String,
    #[serde(rename = "CreaterDate", default)]
    pub creater_at: String,
    #[serde(rename = "Longitude", default)]
    pub longitude: String,
    #[serde(rename = "Latitude", default)]
    pub latitude: String,
}

#[derive(Debug, Clone)]
pub struct SignActivity {
    pub id: String,
    pub r#type: SignType,
    pub code: Option<String>,
    pub coordinate: Option<(f64, f64)>,
}

fn parse_coordinate(longitude: &str, latitude: &str) -> Option<(f64, f64)> {
    match (longitude.parse::<f64>(), latitude.parse::<f64>()) {
        (Ok(longitude), Ok(latitude)) => Some((longitude, latitude)),
        _ => None,
    }
}

impl SignActivity {
    pub(crate) fn from_raw(raw: &RawRow) -> SignActivity {
        let code = if raw.checkin_code.is_empty() {
            None
        } else {
            Some(raw.checkin_code.clone())
        };

        SignActivity {
            id: raw.id.clone(),
            r#type: SignType::parse(&raw.checkin_type),
            code,
            coordinate: parse_coordinate(&raw.longitude, &raw.latitude),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> RawRow {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn checkin_type_maps_to_sign_type() {
        let code = SignActivity::from_raw(&raw(r#"{"ID":"1","CheckInType":"1"}"#));
        assert!(matches!(code.r#type, SignType::Code));
        let qr = SignActivity::from_raw(&raw(r#"{"ID":"2","CheckInType":"2"}"#));
        assert!(matches!(qr.r#type, SignType::Qr));
        let locate = SignActivity::from_raw(&raw(r#"{"ID":"3","CheckInType":"3"}"#));
        assert!(matches!(locate.r#type, SignType::Locate));
        let hanzi = SignActivity::from_raw(&raw(r#"{"ID":"5","CheckInType":"4"}"#));
        assert!(matches!(hanzi.r#type, SignType::Hanzi));
        let unknown = SignActivity::from_raw(&raw(r#"{"ID":"4","CheckInType":"9"}"#));
        assert!(matches!(unknown.r#type, SignType::Unknown));
    }

    #[test]
    fn empty_code_translates_to_none() {
        let activity = SignActivity::from_raw(&raw(r#"{"ID":"1","CheckInCode":""}"#));
        assert!(activity.code.is_none());
    }

    #[test]
    fn code_is_kept_when_present() {
        let activity = SignActivity::from_raw(&raw(r#"{"ID":"1","CheckInCode":"8888"}"#));
        assert_eq!(activity.code.as_deref(), Some("8888"));
    }

    #[test]
    fn coordinate_requires_both_fields() {
        let both = raw(r#"{"ID":"1","Longitude":"114.39437","Latitude":"22.70462"}"#);
        let activity = SignActivity::from_raw(&both);
        assert_eq!(activity.coordinate, Some((114.39437, 22.70462)));
        let half = raw(r#"{"ID":"1","Longitude":"114.39437"}"#);
        let activity = SignActivity::from_raw(&half);
        assert_eq!(activity.coordinate, None);
    }
}
