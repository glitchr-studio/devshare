//! The resolver inside the tunnel. It only knows the hostnames of the
//! session; the device keeps its usual resolver for everything else.

use simple_dns::{
    rdata::{RData, A},
    Packet, PacketFlag, ResourceRecord, CLASS, QTYPE, RCODE, TYPE,
};

use super::AddressPlan;

/// Seconds an answer may be cached. Short: the addresses mean nothing once
/// the session is over.
const TTL: u32 = 5;

/// Answers one query, or nothing if it cannot be parsed.
pub fn answer(query: &[u8], plan: &AddressPlan) -> Option<Vec<u8>> {
    let query = Packet::parse(query).ok()?;
    let question = query.questions.first()?.clone();

    let mut reply = Packet::new_reply(query.id());
    reply.set_flags(PacketFlag::AUTHORITATIVE_ANSWER | PacketFlag::RECURSION_AVAILABLE);
    if query.has_flags(PacketFlag::RECURSION_DESIRED) {
        reply.set_flags(PacketFlag::RECURSION_DESIRED);
    }

    match plan.address_of(&question.qname.to_string()) {
        Some(address) => {
            let wants_ipv4 = matches!(question.qtype, QTYPE::TYPE(TYPE::A) | QTYPE::ANY);
            // Any other record type gets an empty answer, not an error: the
            // name exists, it just has no IPv6 address or mail server.
            if wants_ipv4 {
                reply.answers.push(ResourceRecord::new(
                    question.qname.clone(),
                    CLASS::IN,
                    TTL,
                    RData::A(A {
                        address: address.into(),
                    }),
                ));
            }
        }
        None => *reply.rcode_mut() = RCODE::NameError,
    }

    reply.questions.push(question);
    reply.build_bytes_vec().ok()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use simple_dns::{Name, Question};

    use super::*;
    use crate::guest::addresses::tests::manifest;

    fn ask(name: &str, qtype: TYPE) -> Vec<u8> {
        let mut query = Packet::new_query(7);
        query.set_flags(PacketFlag::RECURSION_DESIRED);
        query.questions.push(Question::new(
            Name::new_unchecked(name).into_owned(),
            qtype.into(),
            CLASS::IN.into(),
            false,
        ));
        query.build_bytes_vec().unwrap()
    }

    #[test]
    fn resolves_shared_names_and_nothing_else() {
        let plan = AddressPlan::new(&manifest(&[("shop.test", 443)])).unwrap();

        let bytes = answer(&ask("Shop.TEST", TYPE::A), &plan).unwrap();
        let reply = Packet::parse(&bytes).unwrap();
        assert_eq!(reply.id(), 7);
        assert_eq!(reply.rcode(), RCODE::NoError);
        let RData::A(a) = &reply.answers[0].rdata else {
            panic!("not an A record")
        };
        assert_eq!(
            Ipv4Addr::from(a.address),
            plan.address_of("shop.test").unwrap()
        );

        let bytes = answer(&ask("shop.test", TYPE::AAAA), &plan).unwrap();
        let reply = Packet::parse(&bytes).unwrap();
        assert_eq!(reply.rcode(), RCODE::NoError);
        assert!(reply.answers.is_empty());

        let bytes = answer(&ask("db.shop.test", TYPE::A), &plan).unwrap();
        assert_eq!(Packet::parse(&bytes).unwrap().rcode(), RCODE::NameError);

        assert_eq!(answer(b"junk", &plan), None);
    }
}
