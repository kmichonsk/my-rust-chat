use mio::Token;

pub struct UniqueTokenGenerator {
    internal_token: Token,
}

impl UniqueTokenGenerator {
    pub fn new() -> UniqueTokenGenerator {
        UniqueTokenGenerator {
            internal_token: Token(0),
        }
    }

    pub fn generate(&mut self) -> Token {
        let token = self.internal_token.0;
        self.internal_token.0 += 1;
        Token(token)
    }
}

// TODO: tests
