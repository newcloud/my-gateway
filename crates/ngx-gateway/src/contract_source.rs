use std::fs;

/// 契约响应体上限。契约是配置，正常只有几十 KB；超过上限按拉取失败处理，
/// 避免恶意/配置错误的契约把内存吃光（见 `docs/PROTOCOL.md` §1）。
const MAX_CONTRACT_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct RawContract {
    pub bytes: Vec<u8>,
}

pub trait ContractSource: Send {
    fn fetch(&mut self) -> anyhow::Result<RawContract>;
}

pub struct FileSource {
    path: String,
}

impl FileSource {
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

impl ContractSource for FileSource {
    fn fetch(&mut self) -> anyhow::Result<RawContract> {
        Ok(RawContract {
            bytes: fs::read(&self.path)?,
        })
    }
}

pub struct HttpSource {
    url: String,
    agent: ureq::Agent,
}

impl HttpSource {
    pub fn new(url: String, allow_localhost_ca: bool) -> Self {
        let mut builder = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(3)))
            .max_redirects(0);
        if allow_localhost_ca {
            builder = builder.tls_config(
                ureq::tls::TlsConfig::builder()
                    .disable_verification(true)
                    .build(),
            );
        }
        let agent: ureq::Agent = builder.build().into();
        Self { url, agent }
    }
}

impl ContractSource for HttpSource {
    fn fetch(&mut self) -> anyhow::Result<RawContract> {
        let mut response = self.agent.get(&self.url).call()?;
        // `max_redirects(0)` 下 3xx 会作为普通响应返回；契约地址出现重定向本身就是
        // 不可信信号（可能被劫持到别处），按失败处理，绝不跟随。
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("unexpected HTTP status {}", status.as_u16());
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_CONTRACT_BYTES)
            .read_to_vec()?;
        Ok(RawContract { bytes: body })
    }
}
