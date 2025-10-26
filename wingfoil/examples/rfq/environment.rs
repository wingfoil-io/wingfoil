#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Environment {
    Prod,
    Test,
}

impl Environment {
    fn access_key(&self) -> String {
        let env = match self {
            Self::Prod => "PROD",
            Self::Test => "TEST",
        };
        let env_variable = format!("PARADIGM_{env}_ACCESS_KEY");
        let env_var = env_variable.as_str();
        std::env::var(env_var).unwrap_or_else(|_| panic!("{} not found", env_var))
    }

    pub fn url(&self) -> String {
        let env = match self {
            Self::Prod => "prod",
            Self::Test => "testnet",
        };
        let access_key = self.access_key();
        format!("wss://ws.api.{env}.paradigm.trade/v2/drfq/?api-key={access_key}&cancel_on_disconnect=false")
    }
}
