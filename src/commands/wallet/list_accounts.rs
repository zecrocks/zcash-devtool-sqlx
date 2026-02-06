use clap::Args;
#[cfg(feature = "postgres")]
use uuid::Uuid;
use zcash_client_backend::data_api::{Account, WalletRead};
use zcash_client_sqlite::WalletDb;
use zcash_protocol::consensus::Parameters;

use crate::{config::get_wallet_network, data::get_db_paths};

#[cfg(feature = "postgres")]
use crate::data::DbBackend;

// Options accepted for the `list-accounts` command
#[derive(Debug, Args)]
pub(crate) struct Command {}

fn print_accounts<P: Parameters, DbT: WalletRead>(
    db_data: &DbT,
    params: &P,
) -> anyhow::Result<()>
where
    DbT::Error: std::error::Error + Sync + Send + 'static,
{
    for account_id in db_data.get_account_ids()?.iter() {
        let account = db_data.get_account(*account_id)?.unwrap();

        println!("Account {:?}", account_id);
        if let Some(name) = account.name() {
            println!("     Name: {name}");
        }
        println!("     UIVK: {}", account.uivk().encode(params));
        println!(
            "     UFVK: {}",
            account
                .ufvk()
                .map_or("None".to_owned(), |k| k.encode(params))
        );
        println!("     Source: {:?}", account.source());
    }
    Ok(())
}

impl Command {
    pub(crate) fn run(
        self,
        wallet_dir: Option<String>,
        #[cfg(feature = "postgres")] db_backend: DbBackend,
        #[cfg(feature = "postgres")] pg_wallet_id: Option<Uuid>,
    ) -> anyhow::Result<()> {
        #[cfg(feature = "postgres")]
        if let DbBackend::Postgres(ref url) = db_backend {
            let runtime = tokio::runtime::Handle::current();
            let pool = tokio::task::block_in_place(|| runtime.block_on(zcash_client_sqlx::create_pool_default(url)))?;

            let wallet_id = match pg_wallet_id {
                Some(uuid) => zcash_client_sqlx::WalletId::from_uuid(uuid),
                None => {
                    let wallets = tokio::task::block_in_place(|| runtime.block_on(
                        zcash_client_sqlx::WalletDb::<zcash_protocol::consensus::Network>::list_wallets_async(&pool),
                    ))?;
                    match &wallets[..] {
                        [] => return Err(anyhow::anyhow!("No wallets found.")),
                        [w] => w.id,
                        _ => {
                            println!("Multiple wallets found:");
                            for w in &wallets {
                                println!(
                                    "  {} {}",
                                    w.id.expose_uuid(),
                                    w.name.as_deref().unwrap_or("")
                                );
                            }
                            return Err(anyhow::anyhow!(
                                "Please specify --wallet-id."
                            ));
                        }
                    }
                }
            };

            // Determine network from the wallet info
            let wallets = tokio::task::block_in_place(|| runtime.block_on(
                zcash_client_sqlx::WalletDb::<zcash_protocol::consensus::Network>::list_wallets_async(&pool),
            ))?;
            let wallet_info = wallets
                .iter()
                .find(|w| w.id == wallet_id)
                .ok_or_else(|| anyhow::anyhow!("Wallet not found"))?;
            let params = match wallet_info.network.to_lowercase().as_str() {
                "main" => zcash_protocol::consensus::Network::MainNetwork,
                _ => zcash_protocol::consensus::Network::TestNetwork,
            };

            let db = zcash_client_sqlx::WalletDb::for_wallet_with_handle(
                pool,
                wallet_id,
                params,
                runtime.clone(),
            );

            return print_accounts(&db, &params);
        }

        let params = get_wallet_network(wallet_dir.as_ref())?;
        let (_, db_data) = get_db_paths(wallet_dir.as_ref());
        let db_data = WalletDb::for_path(db_data, params, (), ())?;
        print_accounts(&db_data, &params)
    }
}
