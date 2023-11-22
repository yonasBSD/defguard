use super::{error::OriLDAPError, LDAPConnection};
use crate::db::{DbPool, User};

pub async fn user_from_ldap(
    pool: &DbPool,
    username: &str,
    password: &str,
) -> Result<User, OriLDAPError> {
    let mut ldap_connection = LDAPConnection::create(pool).await?;
    let mut user = ldap_connection.get_user(username, password).await?;
    let _result = user.save(pool).await; // FIXME: do not ignore errors
    Ok(user)
}

pub async fn ldap_add_user(pool: &DbPool, user: &User, password: &str) -> Result<(), OriLDAPError> {
    let mut ldap_connection = LDAPConnection::create(pool).await?;
    match ldap_connection.add_user(user, password).await {
        Ok(()) => Ok(()),
        // this user might exist in LDAP, just try to set the password
        Err(_) => ldap_connection.set_password(&user.username, password).await,
    }
}

pub async fn ldap_modify_user(
    pool: &DbPool,
    username: &str,
    user: &User,
) -> Result<(), OriLDAPError> {
    let mut ldap_connection = LDAPConnection::create(pool).await?;
    ldap_connection.modify_user(username, user).await
}

pub async fn ldap_delete_user(pool: &DbPool, username: &str) -> Result<(), OriLDAPError> {
    let mut ldap_connection = LDAPConnection::create(pool).await?;
    ldap_connection.delete_user(username).await
}

pub async fn ldap_add_user_to_group(
    pool: &DbPool,
    username: &str,
    groupname: &str,
) -> Result<(), OriLDAPError> {
    let mut ldap_connection = LDAPConnection::create(pool).await?;
    ldap_connection.add_user_to_group(username, groupname).await
}

pub async fn ldap_remove_user_from_group(
    pool: &DbPool,
    username: &str,
    groupname: &str,
) -> Result<(), OriLDAPError> {
    let mut ldap_connection = LDAPConnection::create(pool).await?;
    ldap_connection
        .remove_user_from_group(username, groupname)
        .await
}

pub async fn ldap_change_password(
    pool: &DbPool,
    username: &str,
    password: &str,
) -> Result<(), OriLDAPError> {
    let mut ldap_connection = LDAPConnection::create(pool).await?;
    ldap_connection.set_password(username, password).await
}
