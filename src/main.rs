use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::fs::File;
use std::io::Write;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(author, version, about = "VQ-Capital Tearsheet Generator", long_about = None)]
struct Args {
    /// QuestDB HTTP Endpoint
    #[arg(short, long, default_value = "http://localhost:19000")]
    questdb_url: String,

    /// Output Markdown File Path
    #[arg(short, long, default_value = "TEARSHEET.md")]
    output: String,
}

#[derive(Deserialize, Debug)]
struct TradeRecord {
    #[serde(rename = "symbol")]
    _symbol: String,
    #[serde(rename = "side")]
    _side: String,
    #[serde(rename = "order_id")]
    _order_id: String,
    #[serde(rename = "exec_price")]
    _exec_price: f64,
    #[serde(rename = "qty")]
    _qty: f64,
    #[serde(rename = "pnl")]
    pnl: f64,
    #[serde(rename = "latency_ms")]
    latency_ms: f64,
    #[serde(rename = "timestamp")]
    _timestamp: i64, // Mikro saniye
}

#[derive(Deserialize, Debug)]
struct PerformanceRecord {
    #[serde(rename = "equity")]
    _equity: f64,
    #[serde(rename = "balance")]
    _balance: f64,
    #[serde(rename = "unrealized_pnl")]
    _unrealized_pnl: f64,
    #[serde(rename = "drawdown_pct")]
    drawdown_pct: f64,
    #[serde(rename = "sharpe_ratio")]
    _sharpe_ratio: f64,
    #[serde(rename = "timestamp")]
    _timestamp: i64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!(
        "📡 Service: {} | Version: {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );

    let args = Args::parse();

    info!("🦅 VQ-Capital Tearsheet Generator başlatılıyor...");

    // 1. PAPER TRADES İNDİRME VE ANALİZ (O(N) Streaming Parsing)
    info!("📊 İşlem geçmişi (paper_trades) QuestDB'den çekiliyor...");
    let trades_query = "SELECT * FROM paper_trades ORDER BY timestamp ASC";
    let trades_url = format!(
        "{}/exp?query={}",
        args.questdb_url,
        urlencoding::encode(trades_query)
    );

    let trades_csv = reqwest::get(&trades_url)
        .await
        .context("QuestDB'ye bağlanılamadı. Veritabanı ayakta mı?")?
        .text()
        .await
        .context("QuestDB'den trade CSV verisi okunamadı")?;

    let mut trade_reader = csv::Reader::from_reader(trades_csv.as_bytes());

    let mut total_trades = 0;
    let mut winning_trades = 0;
    let mut gross_profit = 0.0;
    let mut gross_loss = 0.0;
    let mut total_latency = 0.0;
    let mut sla_breaches = 0;
    let mut trade_returns: Vec<f64> = Vec::with_capacity(1000);

    for result in trade_reader.deserialize() {
        match result {
            Ok(record) => {
                let trade: TradeRecord = record;
                total_trades += 1;
                total_latency += trade.latency_ms;

                if trade.latency_ms > 50.0 {
                    sla_breaches += 1;
                }

                if trade.pnl > 0.0 {
                    winning_trades += 1;
                    gross_profit += trade.pnl;
                } else {
                    gross_loss += trade.pnl.abs();
                }

                // Sharpe/Sortino hesaplaması için PnL'i havuzda topluyoruz
                trade_returns.push(trade.pnl);
            }
            Err(e) => {
                error!("Satır ayrıştırma hatası, atlanıyor: {}", e);
                continue;
            }
        }
    }

    // 2. PERFORMANCE İNDİRME VE ANALİZ (Max Drawdown Tespiti)
    info!("📉 Performans geçmişi (performance) QuestDB'den çekiliyor...");
    let perf_query = "SELECT * FROM performance";
    let perf_url = format!(
        "{}/exp?query={}",
        args.questdb_url,
        urlencoding::encode(perf_query)
    );

    let perf_csv = reqwest::get(&perf_url)
        .await
        .context("QuestDB Performans verisine ulaşılamadı")?
        .text()
        .await
        .context("QuestDB'den performance CSV okunamadı")?;

    let mut perf_reader = csv::Reader::from_reader(perf_csv.as_bytes());
    let mut max_drawdown_pct = 0.0;

    for record in perf_reader.deserialize().flatten() {
        let perf: PerformanceRecord = record;
        if perf.drawdown_pct > max_drawdown_pct {
            max_drawdown_pct = perf.drawdown_pct;
        }
    }

    // 3. KUANTİTATİF MATEMATİK HESAPLAMALARI
    let net_pnl = gross_profit - gross_loss;
    let win_rate = if total_trades > 0 {
        (winning_trades as f64 / total_trades as f64) * 100.0
    } else {
        0.0
    };
    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else {
        999.0
    };
    let avg_latency = if total_trades > 0 {
        total_latency / total_trades as f64
    } else {
        0.0
    };

    // Sharpe ve Sortino Hesaplamaları
    let (sharpe_ratio, sortino_ratio) = if total_trades > 1 {
        let mean_return = net_pnl / total_trades as f64;

        let mut variance = 0.0;
        let mut downside_variance = 0.0;
        let mut downside_count = 0.0;

        for r in &trade_returns {
            variance += (r - mean_return).powi(2);
            if *r < 0.0 {
                downside_variance += r.powi(2);
                downside_count += 1.0;
            }
        }

        let std_dev = (variance / total_trades as f64).sqrt();
        let downside_std_dev = if downside_count > 0.0 {
            (downside_variance / downside_count).sqrt()
        } else {
            0.0
        };

        // İşlem sayısına dayalı basit katsayı (HFT için yıllıklaştırma yerine işlem başı ölçüm)
        let s_ratio = if std_dev > 0.0 {
            mean_return / std_dev
        } else {
            0.0
        };
        let sort_ratio = if downside_std_dev > 0.0 {
            mean_return / downside_std_dev
        } else {
            0.0
        };

        (s_ratio, sort_ratio)
    } else {
        (0.0, 0.0)
    };

    // 4. MARKDOWN RAPOR OLUŞTURMA
    info!("📝 Tearsheet Raporu oluşturuluyor...");
    let report = format!(
        "
# 🦅 VQ-CAPITAL QUANTITATIVE TEARSHEET
**Date Generated:** `{}`
**Data Source:** `QuestDB (HFT Engine)`

## 1. 📊 EXECUTIVE SUMMARY
| Metric | Value |
|---|---|
| **Total Trades Processed** | `{}` |
| **Win Rate** | `{:.2}%` |
| **Net PnL** | `${:.2}` |
| **Gross Profit** | `${:.2}` |
| **Gross Loss** | `${:.2}` |
| **Profit Factor** | `{:.2}` |

## 2. 📉 RISK & RETURN METRICS
| Metric | Value | Threshold Status |
|---|---|---|
| **Max Drawdown** | `{:.2}%` | {} |
| **Sharpe Ratio** | `{:.2}` | {} |
| **Sortino Ratio** | `{:.2}` | {} |

## 3. ⚡ EXECUTION & SLA METRICS
| Metric | Value | Note |
|---|---|---|
| **Average Latency** | `{:.2} ms` | Target < 25ms |
| **SLA Breaches (>50ms)** | `{}` | Execution Drop Count |

---
*Generated by VQ-Capital Tearsheet Agent (Rust Zero-Allocation Engine).*
",
        chrono::Utc::now().to_rfc2822(),
        total_trades,
        win_rate,
        net_pnl,
        gross_profit,
        gross_loss,
        profit_factor,
        max_drawdown_pct,
        if max_drawdown_pct > 15.0 {
            "🔴 DANGER"
        } else {
            "🟢 SAFE"
        },
        sharpe_ratio,
        if sharpe_ratio > 1.0 {
            "🟢 ALPHA"
        } else {
            "🔴 NOISE"
        },
        sortino_ratio,
        if sortino_ratio > 1.5 {
            "🟢 EXCELLENT"
        } else {
            "🔴 HIGH RISK"
        },
        avg_latency,
        sla_breaches
    );

    let mut file = File::create(&args.output).context("Çıktı dosyası oluşturulamadı")?;
    file.write_all(report.as_bytes())
        .context("Rapor dosyaya yazılamadı")?;

    info!("✅ Tearsheet Başarıyla Kaydedildi -> {}", args.output);

    Ok(())
}
