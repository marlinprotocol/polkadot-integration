use libp2p::identity::Keypair;
use std::error::Error;
use hex;
use libp2p::identity::ed25519;
use std::fs;
use std::path::PathBuf;

pub struct NodeKey {
    pub file: PathBuf,
}

impl NodeKey {
    pub fn generate_key() -> Result<Keypair, Box<dyn Error + Send + Sync>> {
        let id_keys = Keypair::generate_ed25519();
        Ok(id_keys)
    }
}

impl NodeKey {
    pub fn save_raw_key(&self) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let keypair = ed25519::Keypair::generate();
        let secret = keypair.secret();
        let secret_hex = hex::encode(secret.as_ref());
        // let mut file = File::create(self.file.clone())?;
        fs::write(self.file.clone(), secret_hex)?;
        Ok(1)
    }
}

impl NodeKey {
    pub fn load_key(&self) -> Result<ed25519::Keypair, Box<dyn Error + Send + Sync>> {
        let mut file_content =
            hex::decode(fs::read(&self.file)?).map_err(|_| "failed to decode secret as hex")?;
        let secret =
            ed25519::SecretKey::from_bytes(&mut file_content).map_err(|_| "Bad node key file")?;
        let keypair = ed25519::Keypair::from(secret);
        Ok(keypair)
    }
}

impl NodeKey {
    pub fn load_or_generate(
        &self,
        _overwrite: bool,
    ) -> Result<ed25519::Keypair, Box<dyn Error + Send + Sync>> {
        let res = self.load_key();
        match res {
            Ok(_) => res,
            Err(_) => {
                let _result = self.save_raw_key();
                self.load_key()
            }
        }
    }
}
