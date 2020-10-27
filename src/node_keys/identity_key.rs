use libp2p::{identity,identity::{Keypair}};
use rand;
// use libp2pcore;
// use libp2p::secio::SecioKeyPair;
use std::error::Error;
use std::io::prelude::*;
use std::fs::File;
use std::num::NonZeroU32;
use std::path::Path;
use std::fs;
use std::path::PathBuf;
use libp2p::identity::ed25519;
use hex;

pub struct node_key{
    pub file: PathBuf,
}

impl node_key {
    pub fn generate_key() -> Result<Keypair, Box<dyn Error + Send + Sync>> {
        let id_keys = Keypair::generate_ed25519();
        Ok(id_keys)
    }
}

impl node_key {
    pub fn save_raw_key(&self) ->
    Result<usize, Box<dyn Error + Send + Sync>>
    {
        let keypair = ed25519::Keypair::generate();
        let secret = keypair.secret();
        let secret_hex = hex::encode(secret.as_ref());
        // let mut file = File::create(self.file.clone())?;
        fs::write(self.file.clone(), secret_hex)?;
        Ok(1)
    }
}

impl node_key {
    pub fn load_key(&self) ->
    Result<ed25519::Keypair, Box<dyn Error + Send + Sync>>
    {
        let mut file_content = hex::decode(fs::read(&self.file)?)
                                 .map_err(|_| "failed to decode secret as hex")?;
        let secret = ed25519::SecretKey::from_bytes(&mut file_content)
                                .map_err(|_| "Bad node key file")?;
        let keypair = ed25519::Keypair::from(secret);
        Ok(keypair)
    }
}

impl node_key {
    pub fn load_or_generate(&self, overwrite: bool) ->
    Result<ed25519::Keypair, Box<dyn Error + Send + Sync>>
    {
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

/*
#[cfg(test)]
mod tests {

    use std::fs::remove_file;
    use super::{generate_key, generate_raw, load_key,
                load_or_generate,raw_to_key,save_raw_key};

    #[test]
    fn test_generate_key() {
        let key = generate_key();
        println!("public id {:?}", key.unwrap().to_peer_id());
    }

    #[test]
    fn test_save_raw_key() {
        let key = generate_raw();
        let file_name = "tests/keystore_save_test".to_owned();
        let _res = save_raw_key(key, &file_name);
        remove_file(file_name).expect("file remove failure");
    }

    #[test]
    fn test_load_key() {
        let key = load_key(&"tests/keystore".to_owned());
        println!("public id: {:?}", key.unwrap().to_peer_id());
    }

    #[test]
    fn test_save_and_load() {
        let raw_key = generate_raw();
        println!("{:?}", raw_key);
        let file_name = "tests/keystore_save".to_owned();
        let _res = save_raw_key(raw_key, &file_name);
        let key = raw_to_key(raw_key);
        let key2 = load_key(&file_name);
        assert_eq!(key.unwrap().to_peer_id(), key2.unwrap().to_peer_id());
        remove_file(file_name).expect("file remove failure");
    }

    #[test]
    fn test_load_failure() {
        let res = load_key(&"nonexisiting".to_owned());
        assert!(res.is_err());
    }

    #[test]
    fn test_load_or_generate() {
        let file_name = "tests/newkeystore".to_owned();
        let key = load_or_generate(&file_name, true);
        let key2 = load_or_generate(&file_name, true);
        assert_eq!(key.unwrap().to_peer_id(), key2.unwrap().to_peer_id());
        remove_file(file_name).expect("file remove failure");
    }
}
*/