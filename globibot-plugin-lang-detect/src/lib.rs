pub struct LanguageDetector {
    api_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageDetection {
    pub language: String,
    pub is_reliable: bool,
    pub confidence: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum DetectionError {
    #[error("Network error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("API did not return any detection")]
    NoDetections,
}

macro_rules! endpoint {
    ($path: literal) => {
        concat!("https://ws.detectlanguage.com/0.2", $path)
    };
}

impl LanguageDetector {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
        }
    }

    pub async fn detect_language(
        &self,
        sentence: &str,
    ) -> Result<LanguageDetection, DetectionError> {
        #[derive(serde::Serialize)]
        struct Request<'q> {
            q: &'q str,
        }

        #[derive(Debug, serde::Deserialize)]
        struct RawDetectionResponse {
            data: RawDetectionData,
        }

        #[derive(Debug, serde::Deserialize)]
        struct RawDetectionData {
            detections: Vec<LanguageDetection>,
        }

        let client = reqwest::Client::new();

        let response: RawDetectionResponse = client
            .post(endpoint!("/detect"))
            .bearer_auth(&self.api_key)
            .json(&Request { q: sentence })
            .send()
            .await?
            .json()
            .await?;

        let most_confident_detection = response
            .data
            .detections
            .into_iter()
            .max_by(|d1, d2| {
                let reliability_ord = d1.is_reliable.cmp(&d2.is_reliable);
                let confidence_ord = d1
                    .confidence
                    .partial_cmp(&d2.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal);

                reliability_ord.then(confidence_ord)
            })
            .ok_or(DetectionError::NoDetections)?;

        Ok(most_confident_detection)
    }
}

pub fn flag_from_code(lang_code: &str) -> Option<&'static str> {
    let flag = match lang_code {
        // "aa" => "",
        // "ab" => "",
        "af" => "🇦🇫",
        // "ak" => "",
        "am" => "🇦🇲",
        "ar" => "🇦🇷",
        "as" => "🇦🇸",
        // "ay" => "",
        "az" => "🇦🇿",
        "ba" => "🇧🇦",
        "be" => "🇧🇪",
        "bg" => "🇧🇬",
        "bh" => "🇧🇭",
        "bi" => "🇧🇮",
        "bn" => "🇧🇳",
        "bo" => "🇧🇴",
        "br" => "🇧🇷",
        "bs" => "🇧🇸",
        // "bug" => "",
        "ca" => "🇨🇦",
        // "ceb" => "",
        // "chr" => "",
        "co" => "🇨🇴",
        // "crs" => "",
        // "cs" => "",
        "cy" => "🇨🇾",
        // "da" => "",
        "de" => "🇩🇪",
        // "dv" => "",
        "dz" => "🇩🇿",
        // "egy" => "",
        // "el" => "",
        // "en" => "",
        // "eo" => "",
        "es" => "🇪🇸",
        "et" => "🇪🇹",
        "eu" => "🇪🇺",
        // "fa" => "",
        "fi" => "🇫🇮",
        "fj" => "🇫🇯",
        "fo" => "🇫🇴",
        "fr" => "🇫🇷",
        // "fy" => "",
        "ga" => "🇬🇦",
        "gd" => "🇬🇩",
        "gl" => "🇬🇱",
        "gn" => "🇬🇳",
        // "got" => "",
        "gu" => "🇬🇺",
        // "gv" => "",
        // "ha" => "",
        // "haw" => "",
        // "hi" => "",
        // "hmn" => "",
        "hr" => "🇭🇷",
        "ht" => "🇭🇹",
        "hu" => "🇭🇺",
        // "hy" => "",
        // "ia" => "",
        "id" => "🇮🇩",
        "ie" => "🇮🇪",
        // "ig" => "",
        // "ik" => "",
        "is" => "🇮🇸",
        "it" => "🇮🇹",
        // "iu" => "",
        // "iw" => "",
        // "ja" => "",
        // "jw" => "",
        // "ka" => "",
        // "kha" => "",
        // "kk" => "",
        // "kl" => "",
        "km" => "🇰🇲",
        "kn" => "🇰🇳",
        // "ko" => "",
        // "ks" => "",
        // "ku" => "",
        "ky" => "🇰🇾",
        "la" => "🇱🇦",
        "lb" => "🇱🇺",
        // "lg" => "",
        // "lif" => "",
        // "ln" => "",
        // "lo" => "",
        "lt" => "🇱🇹",
        "lv" => "🇱🇻",
        // "mfe" => "",
        "mg" => "🇲🇬",
        // "mi" => "",
        "mk" => "🇲🇰",
        "ml" => "🇲🇱",
        "mn" => "🇲🇳",
        "mr" => "🇲🇷",
        "ms" => "🇲🇸",
        "mt" => "🇲🇹",
        "my" => "🇲🇾",
        "na" => "🇳🇦",
        "ne" => "🇳🇪",
        "nl" => "♿",
        "no" => "🇳🇴",
        "nr" => "🇳🇷",
        // "nso" => "",
        // "ny" => "",
        // "oc" => "",
        "om" => "🇴🇲",
        // "or" => "",
        "pa" => "🇵🇦",
        "pl" => "🇵🇱",
        "ps" => "🇵🇸",
        "pt" => "🇵🇹",
        // "qu" => "",
        // "rm" => "",
        // "rn" => "",
        "ro" => "🇷🇴",
        "ru" => "🇷🇺",
        "rw" => "🇷🇼",
        "sa" => "🇸🇦",
        // "sco" => "",
        "sd" => "🇸🇩",
        "sg" => "🇸🇬",
        "si" => "🇸🇮",
        "sk" => "🇸🇰",
        "sl" => "🇸🇱",
        "sm" => "🇸🇲",
        "sn" => "🇸🇳",
        "so" => "🇸🇴",
        // "sq" => "",
        "sr" => "🇸🇷",
        "ss" => "🇸🇸",
        "st" => "🇸🇹",
        // "su" => "",
        "sv" => "🇸🇻",
        // "sw" => "",
        // "syr" => "",
        "ta" => "🇹🇦",
        // "te" => "",
        "tg" => "🇹🇬",
        "th" => "🇹🇭",
        // "ti" => "",
        "tk" => "🇹🇰",
        "tl" => "🇹🇱",
        // "tlh" => "",
        "tn" => "🇹🇳",
        "to" => "🇹🇴",
        "tr" => "🇹🇷",
        // "ts" => "",
        "tt" => "🇹🇹",
        "ug" => "🇺🇬",
        // "uk" => "",
        // "ur" => "",
        "uz" => "🇺🇿",
        "ve" => "🇻🇪",
        "vi" => "🇻🇮",
        // "vo" => "",
        // "war" => "",
        // "wo" => "",
        // "xh" => "",
        // "yi" => "",
        // "yo" => "",
        "za" => "🇿🇦",
        "zh" => "🇨🇳",
        "zh-Hant" => "🇨🇳",
        // "zu" => "",
        unhandled => {
            eprintln!("unhandled country code: {}", unhandled);
            return None;
        }
    };

    Some(flag)
}
