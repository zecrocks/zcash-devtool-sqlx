use anyhow::anyhow;
use clap::Args;
use iso_currency::Currency;
use rust_decimal::{prelude::FromPrimitive, Decimal};
use tracing::{info, warn};
use uuid::Uuid;
use zcash_client_backend::{
    data_api::{wallet::ConfirmationsPolicy, Account as _, WalletRead},
    tor,
};
use zcash_client_sqlite::WalletDb;
use zcash_keys::keys::UnifiedAddressRequest;
use zcash_protocol::{
    consensus::Parameters,
    value::{Zatoshis, COIN},
};

use crate::{
    commands::select_account, config::get_wallet_network, data::get_db_paths, error,
    parse_currency, remote::tor_client, ui::format_zec,
};

#[cfg(feature = "postgres")]
use crate::data::DbBackend;

// Options accepted for the `balance` command
#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The UUID of the account for which to get a balance
    account_id: Option<Uuid>,

    /// Convert ZEC values into the given currency
    #[arg(long)]
    #[arg(value_parser = parse_currency)]
    convert: Option<Currency>,
}

fn print_balance<P: Parameters, DbT: WalletRead>(
    db_data: &DbT,
    params: &P,
    account_uuid: Option<Uuid>,
    from_uuid: impl Fn(Uuid) -> DbT::AccountId,
    printer: &ValuePrinter,
) -> Result<(), anyhow::Error>
where
    DbT::Error: std::error::Error + Sync + Send + 'static,
{
    let account = select_account(db_data, account_uuid, from_uuid)?;

    let address = db_data
        .get_last_generated_address_matching(account.id(), UnifiedAddressRequest::AllAvailableKeys)?
        .ok_or(error::Error::InvalidRecipient)?;

    if let Some(wallet_summary) = db_data.get_wallet_summary(ConfirmationsPolicy::default())? {
        let balance = wallet_summary
            .account_balances()
            .get(&account.id())
            .ok_or_else(|| anyhow!("Missing account"))?;

        println!("{}", address.encode(params));
        println!("     Height: {}", wallet_summary.chain_tip_height());
        let scan_progress = wallet_summary.progress().scan();
        println!(
            "     Synced: {:0.3}%",
            (*scan_progress.numerator() as f64) * 100f64 / (*scan_progress.denominator() as f64)
        );
        if let Some(progress) = wallet_summary.progress().recovery() {
            println!(
                "     Recovered: {:0.3}%",
                (*progress.numerator() as f64) * 100f64 / (*progress.denominator() as f64)
            );
        }
        println!("    Balance: {}", printer.format(balance.total()));
        println!(
            "     Sapling Spendable: {}",
            printer.format(balance.sapling_balance().spendable_value()),
        );
        println!(
            "     Orchard Spendable: {}",
            printer.format(balance.orchard_balance().spendable_value()),
        );
        #[cfg(feature = "transparent-inputs")]
        println!(
            "  Unshielded Spendable: {}",
            printer.format(balance.unshielded_balance().spendable_value()),
        );
    } else {
        println!("Insufficient information to build a wallet summary.");
    }

    Ok(())
}

impl Command {
    pub(crate) async fn run(
        self,
        wallet_dir: Option<String>,
        #[cfg(feature = "postgres")] db_backend: DbBackend,
        #[cfg(feature = "postgres")] pg_wallet_id: Option<Uuid>,
    ) -> Result<(), anyhow::Error> {
        let printer = if let Some(currency) = self.convert {
            let tor = tor_client(wallet_dir.as_ref()).await?;
            ValuePrinter::with_exchange_rate(&tor, currency).await?
        } else {
            ValuePrinter::ZecOnly
        };

        #[cfg(feature = "postgres")]
        if let DbBackend::Postgres(ref url) = db_backend {
            let pool = zcash_client_sqlx::create_pool_default(url).await?;

            let wallet_id = match pg_wallet_id {
                Some(uuid) => zcash_client_sqlx::WalletId::from_uuid(uuid),
                None => {
                    let wallets =
                        zcash_client_sqlx::WalletDb::<zcash_protocol::consensus::Network>::list_wallets_async(&pool).await?;
                    match &wallets[..] {
                        [] => return Err(anyhow!("No wallets found.")),
                        [w] => w.id,
                        _ => return Err(anyhow!("Multiple wallets found. Please specify --wallet-id.")),
                    }
                }
            };

            let wallets =
                zcash_client_sqlx::WalletDb::<zcash_protocol::consensus::Network>::list_wallets_async(&pool).await?;
            let wallet_info = wallets
                .iter()
                .find(|w| w.id == wallet_id)
                .ok_or_else(|| anyhow!("Wallet not found"))?;
            let params = match wallet_info.network.to_lowercase().as_str() {
                "main" => zcash_protocol::consensus::Network::MainNetwork,
                _ => zcash_protocol::consensus::Network::TestNetwork,
            };

            let db = zcash_client_sqlx::WalletDb::for_wallet_with_handle(
                pool,
                wallet_id,
                params,
                tokio::runtime::Handle::current(),
            );

            return print_balance(&db, &params, self.account_id, zcash_client_sqlx::AccountUuid::from_uuid, &printer);
        }

        let params = get_wallet_network(wallet_dir.as_ref())?;
        let (_, db_data) = get_db_paths(wallet_dir.as_ref());
        let db_data = WalletDb::for_path(db_data, params, (), ())?;
        print_balance(&db_data, &params, self.account_id, zcash_client_sqlite::AccountUuid::from_uuid, &printer)
    }
}

enum ValuePrinter {
    WithConversion { currency: Currency, rate: Decimal },
    ZecOnly,
}

impl ValuePrinter {
    async fn with_exchange_rate(tor: &tor::Client, currency: Currency) -> anyhow::Result<Self> {
        info!("Fetching {:?}/ZEC exchange rate", currency);
        let exchanges = tor::http::cryptex::Exchanges::unauthenticated_known_with_gemini_trusted();
        let usd_zec = tor.get_latest_zec_to_usd_rate(&exchanges).await?;

        if currency == Currency::USD {
            let rate = usd_zec;
            info!("Current {:?}/ZEC exchange rate: {}", currency, rate);
            Ok(Self::WithConversion { currency, rate })
        } else {
            warn!("{:?}/ZEC exchange rate is unsupported", currency);
            Ok(Self::ZecOnly)
        }
    }

    fn format(&self, value: Zatoshis) -> String {
        match self {
            ValuePrinter::WithConversion { currency, rate } => {
                format!(
                    "{} ({}{:.2})",
                    format_zec(value),
                    currency.symbol(),
                    rate * Decimal::from_u64(value.into_u64()).unwrap()
                        / Decimal::from_u64(COIN).unwrap(),
                )
            }
            ValuePrinter::ZecOnly => format_zec(value),
        }
    }
}
