#![warn(clippy::all, clippy::perf, clippy::pedantic, clippy::suspicious)]

pub mod user;
pub mod faction;

mod de_util;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::de::{DeserializeOwned, Error as DeError};
use thiserror::Error;


#[derive(Error, Debug)]
pub enum Error {
    #[error("api returned error '{reason}', code = '{code}'")]
    Api { code: u8, reason: String },

    #[cfg(feature = "reqwest")]
    #[error("api request failed with network error")]
    Reqwest(#[from] reqwest::Error),

    #[cfg(feature = "awc")]
    #[error("api request failed with network error")]
    AwcSend(#[from] awc::error::SendRequestError),

    #[cfg(feature = "awc")]
    #[error("api request failed to read payload")]
    AwcPayload(#[from] awc::error::JsonPayloadError),

    #[error("api response couldn't be deserialized")]
    Deserialize(#[from] serde_json::Error),
}

pub struct ApiResponse {
    value: serde_json::Value,
}

impl ApiResponse {
    fn from_value(mut value: serde_json::Value) -> Result<Self, Error> {
        #[derive(serde::Deserialize)]
        struct ApiErrorDto {
            code: u8,
            #[serde(rename = "error")]
            reason: String,
        }
        match value.get_mut("error") {
            Some(error) => {
                let dto: ApiErrorDto = serde_json::from_value(error.take())?;
                Err(Error::Api {
                    code: dto.code,
                    reason: dto.reason,
                })
            }
            None => Ok(Self { value }),
        }
    }

    fn decode<D>(&self) -> serde_json::Result<D>
    where
        D: DeserializeOwned,
    {
        serde_json::from_value(self.value.clone())
    }

    fn decode_field<D>(&self, field: &'static str) -> serde_json::Result<D>
    where
        D: DeserializeOwned,
    {
        let value = self
            .value
            .get(field)
            .ok_or_else(|| serde_json::Error::missing_field(field))?
            .clone();

        serde_json::from_value(value)
    }
}

pub trait ApiSelection {
    fn raw_value(&self) -> &'static str;

    fn category() -> &'static str;
}

pub trait ApiCategoryResponse {
    type Selection: ApiSelection;

    fn from_response(response: ApiResponse) -> Self;
}

#[async_trait(?Send)]
pub trait ApiClient {
    async fn request(&self, url: String) -> Result<serde_json::Value, Error>;

    fn torn_api(&self, key: String) -> TornApi<Self>
    where
        Self: Sized;
}

#[cfg(feature = "reqwest")]
#[async_trait(?Send)]
impl crate::ApiClient for ::reqwest::Client {
    async fn request(&self, url: String) -> Result<serde_json::Value, crate::Error> {
        let value = self.get(url).send().await?.json().await?;
        Ok(value)
    }

    fn torn_api(&self, key: String) -> crate::TornApi<Self>
    where
        Self: Sized,
    {
        crate::TornApi::from_client(self, key)
    }
}

#[cfg(feature = "awc")]
#[async_trait(?Send)]
impl crate::ApiClient for awc::Client {
    async fn request(&self, url: String) -> Result<serde_json::Value, crate::Error> {
        let value = self.get(url).send().await?.json().await?;
        Ok(value)
    }

    fn torn_api(&self, key: String) -> crate::TornApi<Self>
    where
        Self: Sized,
    {
        crate::TornApi::from_client(self, key)
    }
}

pub struct TornApi<'client, C>
where
    C: ApiClient,
{
    client: &'client C,
    key: String,
}

impl<'client, C> TornApi<'client, C>
where
    C: ApiClient,
{
    #[allow(dead_code)]
    pub(crate) fn from_client(client: &'client C, key: String) -> Self {
        Self { client, key }
    }

    #[must_use]
    pub fn user(self, id: Option<u64>) -> ApiRequestBuilder<'client, C, user::Response> {
        ApiRequestBuilder::new(self.client, self.key, id)
    }

    #[must_use]
    pub fn faction(self, id: Option<u64>) -> ApiRequestBuilder<'client, C, faction::Response> {
        ApiRequestBuilder::new(self.client, self.key, id)
    }
}

pub struct ApiRequestBuilder<'client, C, A>
where
    C: ApiClient,
    A: ApiCategoryResponse,
{
    client: &'client C,
    key: String,
    phantom: std::marker::PhantomData<A>,
    selections: Vec<&'static str>,
    id: Option<u64>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    comment: Option<String>,
}

impl<'client, C, A> ApiRequestBuilder<'client, C, A>
where
    C: ApiClient,
    A: ApiCategoryResponse,
{
    pub(crate) fn new(client: &'client C, key: String, id: Option<u64>) -> Self {
        Self {
            client,
            key,
            phantom: std::marker::PhantomData,
            selections: Vec::new(),
            id,
            from: None,
            to: None,
            comment: None,
        }
    }

    #[must_use]
    pub fn selections(mut self, selections: &[A::Selection]) -> Self {
        self.selections
            .append(&mut selections.iter().map(ApiSelection::raw_value).collect());
        self
    }

    #[must_use]
    pub fn from(mut self, from: DateTime<Utc>) -> Self {
        self.from = Some(from);
        self
    }

    #[must_use]
    pub fn to(mut self, to: DateTime<Utc>) -> Self {
        self.to = Some(to);
        self
    }

    #[must_use]
    pub fn comment(mut self, comment: String) -> Self {
        self.comment = Some(comment);
        self
    }

    /// Executes the api request.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use torn_api::{ApiClient, Error};
    /// use reqwest::Client;
    /// # async {
    ///
    /// let key = "XXXXXXXXX".to_owned();
    /// let response = Client::new()
    ///     .torn_api(key)
    ///     .user(None)
    ///     .send()
    ///     .await;
    ///
    /// // invalid key
    /// assert!(matches!(response, Err(Error::Api { code: 2, .. })));
    /// # };
    /// ```
    ///
    /// # Errors
    ///
    /// Will return an `Err` if the API returns an API error, the request fails due to a network
    /// error, or if the response body doesn't contain valid json.
    pub async fn send(self) -> Result<A, Error> {
        let mut query_fragments = vec![
            format!("selections={}", self.selections.join(",")),
            format!("key={}", self.key),
        ];

        if let Some(from) = self.from {
            query_fragments.push(format!("from={}", from.timestamp()));
        }

        if let Some(to) = self.to {
            query_fragments.push(format!("to={}", to.timestamp()));
        }

        if let Some(comment) = self.comment {
            query_fragments.push(format!("comment={}", comment));
        }

        let query = query_fragments.join("&");

        let id_fragment = match self.id {
            Some(id) => id.to_string(),
            None => "".to_owned(),
        };

        let url = format!(
            "https://api.torn.com/{}/{}?{}",
            A::Selection::category(),
            id_fragment,
            query
        );

        let value = self.client.request(url).await?;

        ApiResponse::from_value(value).map(A::from_response)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Once;

    #[cfg(feature = "reqwest")]
    pub use reqwest::Client;
    #[cfg(all(not(feature = "reqwest"), feature = "awc"))]
    pub use awc::Client;

    #[cfg(feature = "reqwest")]
    pub use tokio::test as async_test;
    #[cfg(all(not(feature = "reqwest"), feature = "awc"))]
    pub use actix_rt::test as async_test;

    use super::*;

    static INIT: Once = Once::new();

    pub(crate) fn setup() -> String {
        INIT.call_once(|| {
            dotenv::dotenv().ok();
        });
        std::env::var("APIKEY").expect("api key")
    }

    #[test]
    fn selection_raw_value() {
        assert_eq!(user::Selection::Basic.raw_value(), "basic");
    }

    #[cfg(feature = "reqwest")]
    #[tokio::test]
    async fn reqwest() {
        let key = setup();

        reqwest::Client::default()
            .torn_api(key)
            .user(None)
            .send()
            .await
            .unwrap();
    }

    #[cfg(feature = "awc")]
    #[actix_rt::test]
    async fn awc() {
        let key = setup();

        awc::Client::default()
            .torn_api(key)
            .user(None)
            .send()
            .await
            .unwrap();
    }
}
