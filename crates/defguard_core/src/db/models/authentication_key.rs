use model_derive::Model;
use sqlx::{query_as, Error as SqlxError, PgExecutor, Type};

use crate::db::{Id, NoId};

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[sqlx(type_name = "authentication_key_type", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AuthenticationKeyType {
    Ssh,
    Gpg,
}

#[derive(Deserialize, Model, Serialize)]
#[table(authentication_key)]
pub(crate) struct AuthenticationKey<I = NoId> {
    id: I,
    pub yubikey_id: Option<i64>,
    pub name: Option<String>,
    pub user_id: Id,
    pub key: String,
    #[model(enum)]
    key_type: AuthenticationKeyType,
}

impl AuthenticationKey {
    #[must_use]
    pub fn new(
        user_id: Id,
        key: String,
        name: Option<String>,
        key_type: AuthenticationKeyType,
        yubikey_id: Option<i64>,
    ) -> Self {
        Self {
            id: NoId,
            yubikey_id,
            user_id,
            key,
            name,
            key_type,
        }
    }
}

impl AuthenticationKey<Id> {
    pub async fn find_by_user_id<'e, E>(
        executor: E,
        user_id: Id,
        key_type: Option<AuthenticationKeyType>,
    ) -> Result<Vec<Self>, SqlxError>
    where
        E: PgExecutor<'e>,
    {
        match key_type {
            Some(key_type) => {
                query_as!(
                    Self,
                    "SELECT id, user_id, yubikey_id \"yubikey_id?\", key, \
                    name, key_type \"key_type: AuthenticationKeyType\" \
                    FROM authentication_key WHERE user_id = $1 AND key_type = $2",
                    user_id,
                    &key_type as &AuthenticationKeyType
                )
                .fetch_all(executor)
                .await
            }
            None => {
                query_as!(
                    Self,
                    "SELECT id, user_id, yubikey_id \"yubikey_id?\", key, \
                    name, key_type \"key_type: AuthenticationKeyType\" \
                    FROM authentication_key WHERE user_id = $1",
                    user_id
                )
                .fetch_all(executor)
                .await
            }
        }
    }
}
