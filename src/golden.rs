macro_rules! golden {
    ($file:literal) => {
        $crate::golden::decode(
            $file,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/golden/",
                $file
            )),
        )
    };
}

pub(crate) use golden;

pub(crate) fn decode<T: serde::de::DeserializeOwned>(file: &str, json: &str) -> T {
    serde_json::from_str(json).unwrap_or_else(|error| panic!("testdata/golden/{file}: {error}"))
}
