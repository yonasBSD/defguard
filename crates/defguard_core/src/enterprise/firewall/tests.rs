use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use chrono::{DateTime, NaiveDateTime};
use ipnetwork::{IpNetwork, Ipv6Network};
use rand::{thread_rng, Rng};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    query, PgPool,
};

use super::{
    find_largest_subnet_in_range, get_last_ip_in_v6_subnet, get_source_users, merge_addrs,
    merge_port_ranges, process_destination_addrs,
};
use crate::{
    db::{
        models::device::{DeviceType, WireguardNetworkDevice},
        setup_pool, Device, Group, Id, NoId, User, WireguardNetwork,
    },
    enterprise::{
        db::models::acl::{
            AclAlias, AclRule, AclRuleAlias, AclRuleDestinationRange, AclRuleDevice, AclRuleGroup,
            AclRuleInfo, AclRuleNetwork, AclRuleUser, AliasKind, PortRange, RuleState,
        },
        firewall::{get_source_addrs, get_source_network_devices},
    },
    grpc::proto::enterprise::firewall::{
        ip_address::Address, port::Port as PortInner, FirewallPolicy, IpAddress, IpRange,
        IpVersion, Port, PortRange as PortRangeProto, Protocol,
    },
};

impl Default for AclRuleDestinationRange<Id> {
    fn default() -> Self {
        Self {
            id: Id::default(),
            rule_id: Id::default(),
            start: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            end: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        }
    }
}

fn random_user_with_id<R: Rng>(rng: &mut R, id: Id) -> User<Id> {
    let mut user: User<Id> = rng.gen();
    user.id = id;
    user
}

fn random_network_device_with_id<R: Rng>(rng: &mut R, id: Id) -> Device<Id> {
    let mut device: Device<Id> = rng.gen();
    device.id = id;
    device.device_type = DeviceType::Network;
    device
}

#[test]
fn test_get_relevant_users() {
    let mut rng = thread_rng();

    // prepare allowed and denied users lists with shared elements
    let user_1 = random_user_with_id(&mut rng, 1);
    let user_2 = random_user_with_id(&mut rng, 2);
    let user_3 = random_user_with_id(&mut rng, 3);
    let user_4 = random_user_with_id(&mut rng, 4);
    let user_5 = random_user_with_id(&mut rng, 5);
    let allowed_users = vec![user_1.clone(), user_2.clone(), user_4.clone()];
    let denied_users = vec![user_3.clone(), user_4, user_5.clone()];

    let users = get_source_users(allowed_users, &denied_users);
    assert_eq!(users, vec![user_1, user_2]);
}

#[test]
fn test_get_relevant_network_devices() {
    let mut rng = thread_rng();

    // prepare allowed and denied network devices lists with shared elements
    let device_1 = random_network_device_with_id(&mut rng, 1);
    let device_2 = random_network_device_with_id(&mut rng, 2);
    let device_3 = random_network_device_with_id(&mut rng, 3);
    let device_4 = random_network_device_with_id(&mut rng, 4);
    let device_5 = random_network_device_with_id(&mut rng, 5);
    let allowed_devices = vec![
        device_1.clone(),
        device_3.clone(),
        device_4.clone(),
        device_5.clone(),
    ];
    let denied_devices = vec![device_2.clone(), device_4, device_5.clone()];

    let devices = get_source_network_devices(allowed_devices, &denied_devices);
    assert_eq!(devices, vec![device_1, device_3]);
}

#[test]
fn test_process_source_addrs_v4() {
    // Test data with mixed IPv4 and IPv6 addresses
    let user_device_ips = vec![
        IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 1, 2)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 1, 5)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)), // Should be filtered out
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
    ];

    let network_device_ips = vec![
        IpAddr::V4(Ipv4Addr::new(10, 0, 1, 3)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 1, 4)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2)), // Should be filtered out
        IpAddr::V4(Ipv4Addr::new(172, 16, 1, 1)),
    ];

    let source_addrs = get_source_addrs(user_device_ips, network_device_ips, IpVersion::Ipv4);

    // Should merge consecutive IPs into ranges and keep separate non-consecutive ranges
    assert_eq!(
        source_addrs,
        [
            IpAddress {
                address: Some(Address::Ip("10.0.1.1".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("10.0.1.2/31".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("10.0.1.4/31".to_string()))
            },
            IpAddress {
                address: Some(Address::Ip("172.16.1.1".to_string())),
            },
            IpAddress {
                address: Some(Address::Ip("192.168.1.100".to_string())),
            },
        ]
    );

    // Test with empty input
    let empty_addrs = get_source_addrs(Vec::new(), Vec::new(), IpVersion::Ipv4);
    assert!(empty_addrs.is_empty());

    // Test with only IPv6 addresses - should return empty result for IPv4
    let ipv6_only = get_source_addrs(
        vec![IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1))],
        vec![IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2))],
        IpVersion::Ipv4,
    );
    assert!(ipv6_only.is_empty());
}

#[test]
fn test_process_source_addrs_v6() {
    // Test data with mixed IPv4 and IPv6 addresses
    let user_device_ips = vec![
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 5)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), // Should be filtered out
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 0, 0, 0, 1)),
    ];

    let network_device_ips = vec![
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 3)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 4)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)), // Should be filtered out
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 2, 0, 0, 0, 1)),
    ];

    let source_addrs = get_source_addrs(user_device_ips, network_device_ips, IpVersion::Ipv6);

    // Should merge consecutive IPs into ranges and keep separate non-consecutive ranges
    assert_eq!(
        source_addrs,
        [
            IpAddress {
                address: Some(Address::Ip("2001:db8::1".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8::2/127".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8::4/127".to_string()))
            },
            IpAddress {
                address: Some(Address::Ip("2001:db8:0:1::1".to_string())),
            },
            IpAddress {
                address: Some(Address::Ip("2001:db8:0:2::1".to_string())),
            },
        ]
    );

    // Test with empty input
    let empty_addrs = get_source_addrs(Vec::new(), Vec::new(), IpVersion::Ipv6);
    assert!(empty_addrs.is_empty());

    // Test with only IPv4 addresses - should return empty result for IPv6
    let ipv4_only = get_source_addrs(
        vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))],
        vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))],
        IpVersion::Ipv6,
    );
    assert!(ipv4_only.is_empty());
}

#[test]
fn test_process_destination_addrs_v4() {
    // Test data with mixed IPv4 and IPv6 networks
    let destination_ips = [
        "10.0.1.0/24".parse().unwrap(),
        "10.0.2.0/24".parse().unwrap(),
        "2001:db8::/64".parse().unwrap(), // Should be filtered out
        "192.168.1.0/24".parse().unwrap(),
    ];

    let destination_ranges = [
        AclRuleDestinationRange {
            start: IpAddr::V4(Ipv4Addr::new(10, 0, 3, 255)),
            end: IpAddr::V4(Ipv4Addr::new(10, 0, 4, 0)),
            ..Default::default()
        },
        AclRuleDestinationRange {
            start: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)), // Should be filtered out
            end: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 100)),
            ..Default::default()
        },
    ];

    let destination_addrs = process_destination_addrs(&destination_ips, &destination_ranges);

    assert_eq!(
        destination_addrs.0,
        [
            IpAddress {
                address: Some(Address::IpSubnet("10.0.1.0/24".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("10.0.2.0/24".to_string())),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.3.255".to_string(),
                    end: "10.0.4.0".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpSubnet("192.168.1.0/24".to_string())),
            },
        ]
    );

    // Test with empty input
    let empty_addrs = process_destination_addrs(&[], &[]);
    assert!(empty_addrs.0.is_empty());

    // Test with only IPv6 addresses - should return empty result for IPv4
    let ipv6_only = process_destination_addrs(&["2001:db8::/64".parse().unwrap()], &[]);
    assert!(ipv6_only.0.is_empty());
}

#[test]
fn test_process_destination_addrs_v6() {
    // Test data with mixed IPv4 and IPv6 networks
    let destination_ips = vec![
        "2001:db8:1::/64".parse().unwrap(),
        "2001:db8:2::/64".parse().unwrap(),
        "10.0.1.0/24".parse().unwrap(), // Should be filtered out
        "2001:db8:3::/64".parse().unwrap(),
    ];

    let destination_ranges = vec![
        AclRuleDestinationRange {
            start: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 4, 0, 0, 0, 0, 1)),
            end: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 4, 0, 0, 0, 0, 3)),
            ..Default::default()
        },
        AclRuleDestinationRange {
            start: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), // Should be filtered out
            end: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            ..Default::default()
        },
    ];

    let destination_addrs = process_destination_addrs(&destination_ips, &destination_ranges);

    assert_eq!(
        destination_addrs.1,
        [
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8:1::/64".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8:2::/64".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8:3::/64".to_string())),
            },
            IpAddress {
                address: Some(Address::Ip("2001:db8:4::1".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8:4::2/127".to_string()))
            }
        ]
    );

    // Test with empty input
    let empty_addrs = process_destination_addrs(&[], &[]);
    assert!(empty_addrs.1.is_empty());

    // Test with only IPv4 addresses - should return empty result for IPv6
    let ipv4_only = process_destination_addrs(&["192.168.1.0/24".parse().unwrap()], &[]);
    assert!(ipv4_only.1.is_empty());
}

#[test]
fn test_merge_v4_addrs() {
    let addr_ranges = vec![
        IpAddr::V4(Ipv4Addr::new(10, 0, 60, 20))..=IpAddr::V4(Ipv4Addr::new(10, 0, 60, 25)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 10, 1))..=IpAddr::V4(Ipv4Addr::new(10, 0, 10, 22)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 8, 127))..=IpAddr::V4(Ipv4Addr::new(10, 0, 9, 12)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 9, 1))..=IpAddr::V4(Ipv4Addr::new(10, 0, 10, 12)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 9, 20))..=IpAddr::V4(Ipv4Addr::new(10, 0, 10, 31)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 0, 20))..=IpAddr::V4(Ipv4Addr::new(192, 168, 0, 20)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 20, 20))..=IpAddr::V4(Ipv4Addr::new(10, 0, 20, 20)),
    ];

    let merged_addrs = merge_addrs(addr_ranges);

    assert_eq!(
        merged_addrs,
        [
            IpAddress {
                address: Some(Address::Ip("10.0.8.127".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("10.0.8.128/25".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("10.0.9.0/24".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("10.0.10.0/27".to_string())),
            },
            IpAddress {
                address: Some(Address::Ip("10.0.20.20".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("10.0.60.20/30".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("10.0.60.24/31".to_string())),
            },
            IpAddress {
                address: Some(Address::Ip("192.168.0.20".to_string())),
            },
        ]
    );

    // merge single IPs into a range
    let addr_ranges = vec![
        IpAddr::V4(Ipv4Addr::new(10, 0, 10, 0))..=IpAddr::V4(Ipv4Addr::new(10, 0, 10, 0)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 10, 1))..=IpAddr::V4(Ipv4Addr::new(10, 0, 10, 1)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 10, 2))..=IpAddr::V4(Ipv4Addr::new(10, 0, 10, 2)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 10, 3))..=IpAddr::V4(Ipv4Addr::new(10, 0, 10, 3)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 10, 20))..=IpAddr::V4(Ipv4Addr::new(10, 0, 10, 20)),
    ];

    let merged_addrs = merge_addrs(addr_ranges);
    assert_eq!(
        merged_addrs,
        [
            IpAddress {
                address: Some(Address::IpSubnet("10.0.10.0/30".to_string())),
            },
            IpAddress {
                address: Some(Address::Ip("10.0.10.20".to_string())),
            },
        ]
    );
}

#[test]
fn test_merge_v6_addrs() {
    let addr_ranges = vec![
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x1, 0x0, 0x0, 0x0, 0x0, 0x1))
            ..=IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x1, 0x0, 0x0, 0x0, 0x0, 0x5)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x1, 0x0, 0x0, 0x0, 0x0, 0x3))
            ..=IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x1, 0x0, 0x0, 0x0, 0x0, 0x8)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x2, 0x0, 0x0, 0x0, 0x0, 0x1))
            ..=IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x2, 0x0, 0x0, 0x0, 0x0, 0x1)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x3, 0x0, 0x0, 0x0, 0x0, 0x1))
            ..=IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x3, 0x0, 0x0, 0x0, 0x0, 0x3)),
    ];

    let merged_addrs = merge_addrs(addr_ranges);
    assert_eq!(
        merged_addrs,
        [
            IpAddress {
                address: Some(Address::Ip("2001:db8:1::1".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8:1::2/127".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8:1::4/126".to_string()))
            },
            IpAddress {
                address: Some(Address::Ip("2001:db8:1::8".to_string()))
            },
            IpAddress {
                address: Some(Address::Ip("2001:db8:2::1".to_string()))
            },
            IpAddress {
                address: Some(Address::Ip("2001:db8:3::1".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8:3::2/127".to_string()))
            }
        ]
    );
}

#[test]
fn test_merge_addrs_extracts_ipv4_subnets() {
    let ranges = vec![
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0))..=IpAddr::V4(Ipv4Addr::new(192, 168, 2, 255)),
    ];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [
            IpAddress {
                address: Some(Address::IpSubnet("192.168.1.0/24".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("192.168.2.0/24".to_string()))
            },
        ]
    );
}

#[test]
fn test_merge_addrs_extracts_ipv6_subnets() {
    let start = "2001:db8::".parse::<Ipv6Addr>().unwrap();
    let end = "2001:db9::ffff".parse::<Ipv6Addr>().unwrap();
    let ranges = vec![IpAddr::V6(start)..=IpAddr::V6(end)];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8::/32".to_string()))
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db9::/112".to_string()))
            },
        ]
    );
}

#[test]
fn test_merge_addrs_falls_back_to_range_when_no_subnet_fits() {
    let ranges = vec![
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 255))..=IpAddr::V4(Ipv4Addr::new(192, 168, 2, 0)),
    ];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [IpAddress {
            address: Some(Address::IpRange(IpRange {
                start: "192.168.1.255".to_string(),
                end: "192.168.2.0".to_string(),
            })),
        },]
    );

    let start = "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff"
        .parse::<Ipv6Addr>()
        .unwrap();
    let end = "2001:db9::".parse::<Ipv6Addr>().unwrap();
    let ranges = vec![IpAddr::V6(start)..=IpAddr::V6(end)];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [IpAddress {
            address: Some(Address::IpRange(IpRange {
                start: "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff".to_string(),
                end: "2001:db9::".to_string(),
            })),
        },]
    );
}

#[test]
fn test_merge_addrs_handles_single_ip() {
    // Test case: single IP should remain as IP
    let ranges =
        vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))..=IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [IpAddress {
            address: Some(Address::Ip("192.168.1.1".to_string())),
        },]
    );

    let start = "2001:db8::".parse::<Ipv6Addr>().unwrap();
    let end = "2001:db8::".parse::<Ipv6Addr>().unwrap();
    let ranges = vec![IpAddr::V6(start)..=IpAddr::V6(end)];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [IpAddress {
            address: Some(Address::Ip("2001:db8::".to_string())),
        },]
    );
}

#[test]
fn test_find_largest_ipv4_subnet_perfect_match() {
    // Test /24 subnet
    let start = Ipv4Addr::new(192, 168, 1, 0);
    let end = Ipv4Addr::new(192, 168, 1, 255);

    let result = find_largest_subnet_in_range(IpAddr::V4(start), IpAddr::V4(end));

    assert!(result.is_some());
    let subnet = result.unwrap();
    assert_eq!(subnet.to_string(), "192.168.1.0/24");

    // Test /28 subnet (16 addresses)
    let start = Ipv4Addr::new(192, 168, 1, 0);
    let end = Ipv4Addr::new(192, 168, 1, 15);

    let result = find_largest_subnet_in_range(IpAddr::V4(start), IpAddr::V4(end));

    assert!(result.is_some());
    let subnet = result.unwrap();
    assert_eq!(subnet.to_string(), "192.168.1.0/28");
}

#[test]
fn test_find_largest_ipv6_subnet_perfect_match() {
    // Test /112 subnet
    let start = "2001:db8::".parse::<Ipv6Addr>().unwrap();
    let end = "2001:db8::ffff".parse::<Ipv6Addr>().unwrap();

    let result = find_largest_subnet_in_range(IpAddr::V6(start), IpAddr::V6(end));

    assert!(result.is_some());
    let subnet = result.unwrap();
    assert_eq!(subnet.to_string(), "2001:db8::/112");
}

#[test]
fn test_find_largest_subnet_mixed_ip_versions() {
    // Test mixed IP versions should return None
    let start = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0));
    let end = IpAddr::V6("2001:db8::1".parse().unwrap());

    let result = find_largest_subnet_in_range(start, end);

    assert!(result.is_none());
}

#[test]
fn test_find_largest_subnet_invalid_range() {
    // Test invalid range (start > end) should return None
    let start = Ipv4Addr::new(192, 168, 1, 10);
    let end = Ipv4Addr::new(192, 168, 1, 5);

    let result = find_largest_subnet_in_range(IpAddr::V4(start), IpAddr::V4(end));

    assert!(result.is_none());
}

#[test]
fn test_merge_addrs_subnet_at_start_of_range() {
    let ranges = vec![
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0))..=IpAddr::V4(Ipv4Addr::new(192, 168, 1, 64)),
    ];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [
            IpAddress {
                address: Some(Address::IpSubnet("192.168.1.0/26".to_string())),
            },
            IpAddress {
                address: Some(Address::Ip("192.168.1.64".to_string())),
            },
        ]
    );

    let ranges = vec![
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0))
            ..=IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x40)),
    ];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8::/122".to_string())),
            },
            IpAddress {
                address: Some(Address::Ip("2001:db8::40".to_string())),
            },
        ]
    );
}

#[test]
fn test_merge_addrs_subnet_at_end_of_range() {
    let ranges = vec![
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 15))..=IpAddr::V4(Ipv4Addr::new(192, 168, 1, 31)),
    ];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [
            IpAddress {
                address: Some(Address::Ip("192.168.1.15".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("192.168.1.16/28".to_string())),
            },
        ]
    );

    let ranges = vec![
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x0f))
            ..=IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x1f)),
    ];

    let result = merge_addrs(ranges);

    assert_eq!(
        result,
        [
            IpAddress {
                address: Some(Address::Ip("2001:db8::f".to_string())),
            },
            IpAddress {
                address: Some(Address::IpSubnet("2001:db8::10/124".to_string())),
            },
        ]
    );
}

#[test]
fn test_merge_port_ranges() {
    // single port
    let input_ranges = vec![PortRange::new(100, 100)];
    let merged = merge_port_ranges(input_ranges);
    assert_eq!(
        merged,
        [Port {
            port: Some(PortInner::SinglePort(100))
        }]
    );

    // overlapping ranges
    let input_ranges = vec![
        PortRange::new(100, 200),
        PortRange::new(150, 220),
        PortRange::new(210, 300),
    ];
    let merged = merge_port_ranges(input_ranges);
    assert_eq!(
        merged,
        [Port {
            port: Some(PortInner::PortRange(PortRangeProto {
                start: 100,
                end: 300
            }))
        }]
    );

    // duplicate ranges
    let input_ranges = vec![
        PortRange::new(100, 200),
        PortRange::new(100, 200),
        PortRange::new(150, 220),
        PortRange::new(150, 220),
        PortRange::new(210, 300),
        PortRange::new(210, 300),
        PortRange::new(350, 400),
        PortRange::new(350, 400),
        PortRange::new(350, 400),
    ];
    let merged = merge_port_ranges(input_ranges);
    assert_eq!(
        merged,
        [
            Port {
                port: Some(PortInner::PortRange(PortRangeProto {
                    start: 100,
                    end: 300
                }))
            },
            Port {
                port: Some(PortInner::PortRange(PortRangeProto {
                    start: 350,
                    end: 400
                }))
            }
        ]
    );

    // non-consecutive ranges
    let input_ranges = vec![
        PortRange::new(501, 699),
        PortRange::new(151, 220),
        PortRange::new(210, 300),
        PortRange::new(800, 800),
        PortRange::new(200, 210),
        PortRange::new(50, 50),
    ];
    let merged = merge_port_ranges(input_ranges);
    assert_eq!(
        merged,
        [
            Port {
                port: Some(PortInner::SinglePort(50))
            },
            Port {
                port: Some(PortInner::PortRange(PortRangeProto {
                    start: 151,
                    end: 300
                }))
            },
            Port {
                port: Some(PortInner::PortRange(PortRangeProto {
                    start: 501,
                    end: 699
                }))
            },
            Port {
                port: Some(PortInner::SinglePort(800))
            }
        ]
    );

    // fully contained range
    let input_ranges = vec![PortRange::new(100, 200), PortRange::new(120, 180)];
    let merged = merge_port_ranges(input_ranges);
    assert_eq!(
        merged,
        [Port {
            port: Some(PortInner::PortRange(PortRangeProto {
                start: 100,
                end: 200
            }))
        }]
    );
}

#[test]
fn test_last_ip_in_v6_subnet() {
    let subnet: Ipv6Network = "2001:db8:85a3::8a2e:370:7334/64".parse().unwrap();
    let last_ip = get_last_ip_in_v6_subnet(&subnet);
    assert_eq!(
        last_ip,
        IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0x85a3, 0x0000, 0xffff, 0xffff, 0xffff, 0xffff
        ))
    );

    let subnet: Ipv6Network = "280b:47f8:c9d7:634c:cb35:11f3:14e1:5016/119"
        .parse()
        .unwrap();
    let last_ip = get_last_ip_in_v6_subnet(&subnet);
    assert_eq!(
        last_ip,
        IpAddr::V6(Ipv6Addr::new(
            0x280b, 0x47f8, 0xc9d7, 0x634c, 0xcb35, 0x11f3, 0x14e1, 0x51ff
        ))
    )
}

async fn create_acl_rule(
    pool: &PgPool,
    rule: AclRule,
    locations: Vec<Id>,
    allowed_users: Vec<Id>,
    denied_users: Vec<Id>,
    allowed_groups: Vec<Id>,
    denied_groups: Vec<Id>,
    allowed_network_devices: Vec<Id>,
    denied_network_devices: Vec<Id>,
    destination_ranges: Vec<(IpAddr, IpAddr)>,
    aliases: Vec<Id>,
) -> AclRuleInfo<Id> {
    let mut conn = pool.acquire().await.unwrap();

    // create base rule
    let rule = rule.save(&mut *conn).await.unwrap();
    let rule_id = rule.id;

    // create related objects
    // locations
    for location_id in locations {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id,
            network_id: location_id,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // allowed users
    for user_id in allowed_users {
        let obj = AclRuleUser {
            id: NoId,
            allow: true,
            rule_id,
            user_id,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // denied users
    for user_id in denied_users {
        let obj = AclRuleUser {
            id: NoId,
            allow: false,
            rule_id,
            user_id,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // allowed groups
    for group_id in allowed_groups {
        let obj = AclRuleGroup {
            id: NoId,
            allow: true,
            rule_id,
            group_id,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // denied groups
    for group_id in denied_groups {
        let obj = AclRuleGroup {
            id: NoId,
            allow: false,
            rule_id,
            group_id,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // allowed devices
    for device_id in allowed_network_devices {
        let obj = AclRuleDevice {
            id: NoId,
            allow: true,
            rule_id,
            device_id,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // denied devices
    for device_id in denied_network_devices {
        let obj = AclRuleDevice {
            id: NoId,
            allow: false,
            rule_id,
            device_id,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // destination ranges
    for range in destination_ranges {
        let obj = AclRuleDestinationRange {
            id: NoId,
            rule_id,
            start: range.0,
            end: range.1,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // aliases
    for alias_id in aliases {
        let obj = AclRuleAlias {
            id: NoId,
            rule_id,
            alias_id,
        };
        obj.save(&mut *conn).await.unwrap();
    }

    // convert to output format
    rule.to_info(&mut conn).await.unwrap()
}

#[sqlx::test]
async fn test_generate_firewall_rules_ipv4(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;

    let mut rng = thread_rng();

    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: false,
        ..Default::default()
    };
    let mut location = location.save(&pool).await.unwrap();

    // Setup test users and their devices
    let user_1: User<NoId> = rng.gen();
    let user_1 = user_1.save(&pool).await.unwrap();
    let user_2: User<NoId> = rng.gen();
    let user_2 = user_2.save(&pool).await.unwrap();
    let user_3: User<NoId> = rng.gen();
    let user_3 = user_3.save(&pool).await.unwrap();
    let user_4: User<NoId> = rng.gen();
    let user_4 = user_4.save(&pool).await.unwrap();
    let user_5: User<NoId> = rng.gen();
    let user_5 = user_5.save(&pool).await.unwrap();

    for user in [&user_1, &user_2, &user_3, &user_4, &user_5] {
        // Create 2 devices per user
        for device_num in 1..3 {
            let device = Device {
                id: NoId,
                name: format!("device-{}-{}", user.id, device_num),
                user_id: user.id,
                device_type: DeviceType::User,
                description: None,
                wireguard_pubkey: Default::default(),
                created: Default::default(),
                configured: true,
            };
            let device = device.save(&pool).await.unwrap();

            // Add device to location's VPN network
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location.id,
                wireguard_ips: vec![IpAddr::V4(Ipv4Addr::new(
                    10,
                    0,
                    user.id as u8,
                    device_num as u8,
                ))],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
        }
    }

    // Setup test groups
    let group_1 = Group {
        id: NoId,
        name: "group_1".into(),
        ..Default::default()
    };
    let group_1 = group_1.save(&pool).await.unwrap();
    let group_2 = Group {
        id: NoId,
        name: "group_2".into(),
        ..Default::default()
    };
    let group_2 = group_2.save(&pool).await.unwrap();

    // Assign users to groups:
    // Group 1: users 1,2
    // Group 2: users 3,4
    let group_assignments = vec![
        (&group_1, vec![&user_1, &user_2]),
        (&group_2, vec![&user_3, &user_4]),
    ];

    for (group, users) in group_assignments {
        for user in users {
            query!(
                "INSERT INTO group_user (user_id, group_id) VALUES ($1, $2)",
                user.id,
                group.id
            )
            .execute(&pool)
            .await
            .unwrap();
        }
    }

    // Create some network devices
    let network_device_1 = Device {
        id: NoId,
        name: "network-device-1".into(),
        user_id: user_1.id, // Owned by user 1
        device_type: DeviceType::Network,
        description: Some("Test network device 1".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_1 = network_device_1.save(&pool).await.unwrap();

    let network_device_2 = Device {
        id: NoId,
        name: "network-device-2".into(),
        user_id: user_2.id, // Owned by user 2
        device_type: DeviceType::Network,
        description: Some("Test network device 2".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_2 = network_device_2.save(&pool).await.unwrap();

    let network_device_3 = Device {
        id: NoId,
        name: "network-device-3".into(),
        user_id: user_3.id, // Owned by user 3
        device_type: DeviceType::Network,
        description: Some("Test network device 3".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_3 = network_device_3.save(&pool).await.unwrap();

    // Add network devices to location's VPN network
    let network_devices = vec![
        (
            network_device_1.id,
            IpAddr::V4(Ipv4Addr::new(10, 0, 100, 1)),
        ),
        (
            network_device_2.id,
            IpAddr::V4(Ipv4Addr::new(10, 0, 100, 2)),
        ),
        (
            network_device_3.id,
            IpAddr::V4(Ipv4Addr::new(10, 0, 100, 3)),
        ),
    ];

    for (device_id, ip) in network_devices {
        let network_device = WireguardNetworkDevice {
            device_id,
            wireguard_network_id: location.id,
            wireguard_ips: vec![ip],
            preshared_key: None,
            is_authorized: true,
            authorized_at: None,
        };
        network_device.insert(&pool).await.unwrap();
    }

    // Create first ACL rule - Web access
    let acl_rule_1 = AclRule {
        id: NoId,
        name: "Web Access".into(),
        all_networks: false,
        expires: None,
        allow_all_users: false,
        deny_all_users: false,
        allow_all_network_devices: false,
        deny_all_network_devices: false,
        destination: vec!["192.168.1.0/24".parse().unwrap()],
        ports: vec![
            PortRange::new(80, 80).into(),
            PortRange::new(443, 443).into(),
        ],
        protocols: vec![Protocol::Tcp.into()],
        enabled: true,
        parent_id: None,
        state: RuleState::Applied,
    };
    let locations = vec![location.id];
    let allowed_users = vec![user_1.id, user_2.id]; // First two users can access web
    let denied_users = vec![user_3.id]; // Third user explicitly denied
    let allowed_groups = vec![group_1.id]; // First group allowed
    let denied_groups = Vec::new();
    let allowed_devices = vec![network_device_1.id];
    let denied_devices = vec![network_device_2.id, network_device_3.id];
    let destination_ranges = Vec::new();
    let aliases = Vec::new();

    let _acl_rule_1 = create_acl_rule(
        &pool,
        acl_rule_1,
        locations,
        allowed_users,
        denied_users,
        allowed_groups,
        denied_groups,
        allowed_devices,
        denied_devices,
        destination_ranges,
        aliases,
    )
    .await;

    // Create second ACL rule - DNS access
    let acl_rule_2 = AclRule {
        id: NoId,
        name: "DNS Access".into(),
        all_networks: false,
        expires: None,
        allow_all_users: true, // Allow all users
        deny_all_users: false,
        allow_all_network_devices: false,
        deny_all_network_devices: false,
        destination: Vec::new(), // Will use destination ranges instead
        ports: vec![PortRange::new(53, 53).into()],
        protocols: vec![Protocol::Udp.into(), Protocol::Tcp.into()],
        enabled: true,
        parent_id: None,
        state: RuleState::Applied,
    };
    let locations_2 = vec![location.id];
    let allowed_users_2 = Vec::new();
    let denied_users_2 = vec![user_5.id]; // Fifth user denied DNS
    let allowed_groups_2 = Vec::new();
    let denied_groups_2 = vec![group_2.id];
    let allowed_devices_2 = vec![network_device_1.id, network_device_2.id]; // First two network devices allowed
    let denied_devices_2 = vec![network_device_3.id]; // Third network device denied
    let destination_ranges_2 = vec![
        ("10.0.1.13".parse().unwrap(), "10.0.1.43".parse().unwrap()),
        ("10.0.1.52".parse().unwrap(), "10.0.2.43".parse().unwrap()),
    ];
    let aliases_2 = Vec::new();

    let _acl_rule_2 = create_acl_rule(
        &pool,
        acl_rule_2,
        locations_2,
        allowed_users_2,
        denied_users_2,
        allowed_groups_2,
        denied_groups_2,
        allowed_devices_2,
        denied_devices_2,
        destination_ranges_2,
        aliases_2,
    )
    .await;

    let mut conn = pool.acquire().await.unwrap();

    // try to generate firewall config with ACL disabled
    location.acl_enabled = false;
    let generated_firewall_config = location.try_get_firewall_config(&mut conn).await.unwrap();
    assert!(generated_firewall_config.is_none());

    // generate firewall config with default policy Allow
    location.acl_enabled = true;
    location.acl_default_allow = true;
    let generated_firewall_config = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        generated_firewall_config.default_policy,
        i32::from(FirewallPolicy::Allow)
    );

    let generated_firewall_rules = generated_firewall_config.rules;

    assert_eq!(generated_firewall_rules.len(), 4);

    // First ACL - Web Access ALLOW
    let web_allow_rule = &generated_firewall_rules[0];
    assert_eq!(web_allow_rule.verdict, i32::from(FirewallPolicy::Allow));
    assert_eq!(web_allow_rule.protocols, vec![i32::from(Protocol::Tcp)]);
    assert_eq!(
        web_allow_rule.destination_addrs,
        [IpAddress {
            address: Some(Address::IpSubnet("192.168.1.0/24".to_string())),
        }]
    );
    assert_eq!(
        web_allow_rule.destination_ports,
        [
            Port {
                port: Some(PortInner::SinglePort(80))
            },
            Port {
                port: Some(PortInner::SinglePort(443))
            }
        ]
    );
    // Source addresses should include devices of users 1,2 and network_device_1
    assert_eq!(
        web_allow_rule.source_addrs,
        [
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.1.1".to_string(),
                    end: "10.0.1.2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.2.1".to_string(),
                    end: "10.0.2.2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::Ip("10.0.100.1".to_string())),
            },
        ]
    );

    // First ACL - Web Access DENY
    let web_deny_rule = &generated_firewall_rules[2];
    assert_eq!(web_deny_rule.verdict, i32::from(FirewallPolicy::Deny));
    assert!(web_deny_rule.protocols.is_empty());
    assert!(web_deny_rule.destination_ports.is_empty());
    assert!(web_deny_rule.source_addrs.is_empty());
    assert_eq!(
        web_deny_rule.destination_addrs,
        [IpAddress {
            address: Some(Address::IpSubnet("192.168.1.0/24".to_string())),
        }]
    );

    // Second ACL - DNS Access ALLOW
    let dns_allow_rule = &generated_firewall_rules[1];
    assert_eq!(dns_allow_rule.verdict, i32::from(FirewallPolicy::Allow));
    assert_eq!(
        dns_allow_rule.protocols,
        [i32::from(Protocol::Tcp), i32::from(Protocol::Udp)]
    );
    assert_eq!(
        dns_allow_rule.destination_ports,
        [Port {
            port: Some(PortInner::SinglePort(53))
        }]
    );
    // Source addresses should include network_devices 1,2
    assert_eq!(
        dns_allow_rule.source_addrs,
        [
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.1.1".to_string(),
                    end: "10.0.1.2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.2.1".to_string(),
                    end: "10.0.2.2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.100.1".to_string(),
                    end: "10.0.100.2".to_string(),
                })),
            },
        ]
    );

    let expected_destination_addrs = vec![
        IpAddress {
            address: Some(Address::Ip("10.0.1.13".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.14/31".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.16/28".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.32/29".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.40/30".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.52/30".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.56/29".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.64/26".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.128/25".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.2.0/27".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.2.32/29".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.2.40/30".to_string())),
        },
    ];

    assert_eq!(dns_allow_rule.destination_addrs, expected_destination_addrs);

    // Second ACL - DNS Access DENY
    let dns_deny_rule = &generated_firewall_rules[3];
    assert_eq!(dns_deny_rule.verdict, i32::from(FirewallPolicy::Deny));
    assert!(dns_deny_rule.protocols.is_empty(),);
    assert!(dns_deny_rule.destination_ports.is_empty(),);
    assert!(dns_deny_rule.source_addrs.is_empty(),);
    assert_eq!(dns_deny_rule.destination_addrs, expected_destination_addrs);
}

#[sqlx::test]
async fn test_generate_firewall_rules_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    let mut rng = thread_rng();

    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: false,
        address: vec![IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap()],
        ..Default::default()
    };
    let mut location = location.save(&pool).await.unwrap();

    // Setup test users and their devices
    let user_1: User<NoId> = rng.gen();
    let user_1 = user_1.save(&pool).await.unwrap();
    let user_2: User<NoId> = rng.gen();
    let user_2 = user_2.save(&pool).await.unwrap();
    let user_3: User<NoId> = rng.gen();
    let user_3 = user_3.save(&pool).await.unwrap();
    let user_4: User<NoId> = rng.gen();
    let user_4 = user_4.save(&pool).await.unwrap();
    let user_5: User<NoId> = rng.gen();
    let user_5 = user_5.save(&pool).await.unwrap();

    for user in [&user_1, &user_2, &user_3, &user_4, &user_5] {
        // Create 2 devices per user
        for device_num in 1..3 {
            let device = Device {
                id: NoId,
                name: format!("device-{}-{}", user.id, device_num),
                user_id: user.id,
                device_type: DeviceType::User,
                description: None,
                wireguard_pubkey: Default::default(),
                created: Default::default(),
                configured: true,
            };
            let device = device.save(&pool).await.unwrap();

            // Add device to location's VPN network
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location.id,
                wireguard_ips: vec![IpAddr::V6(Ipv6Addr::new(
                    0xff00,
                    0,
                    0,
                    0,
                    0,
                    0,
                    user.id as u16,
                    device_num as u16,
                ))],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
        }
    }

    // Setup test groups
    let group_1 = Group {
        id: NoId,
        name: "group_1".into(),
        ..Default::default()
    };
    let group_1 = group_1.save(&pool).await.unwrap();
    let group_2 = Group {
        id: NoId,
        name: "group_2".into(),
        ..Default::default()
    };
    let group_2 = group_2.save(&pool).await.unwrap();

    // Assign users to groups:
    // Group 1: users 1,2
    // Group 2: users 3,4
    let group_assignments = vec![
        (&group_1, vec![&user_1, &user_2]),
        (&group_2, vec![&user_3, &user_4]),
    ];

    for (group, users) in group_assignments {
        for user in users {
            query!(
                "INSERT INTO group_user (user_id, group_id) VALUES ($1, $2)",
                user.id,
                group.id
            )
            .execute(&pool)
            .await
            .unwrap();
        }
    }

    // Create some network devices
    let network_device_1 = Device {
        id: NoId,
        name: "network-device-1".into(),
        user_id: user_1.id, // Owned by user 1
        device_type: DeviceType::Network,
        description: Some("Test network device 1".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_1 = network_device_1.save(&pool).await.unwrap();

    let network_device_2 = Device {
        id: NoId,
        name: "network-device-2".into(),
        user_id: user_2.id, // Owned by user 2
        device_type: DeviceType::Network,
        description: Some("Test network device 2".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_2 = network_device_2.save(&pool).await.unwrap();

    let network_device_3 = Device {
        id: NoId,
        name: "network-device-3".into(),
        user_id: user_3.id, // Owned by user 3
        device_type: DeviceType::Network,
        description: Some("Test network device 3".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_3 = network_device_3.save(&pool).await.unwrap();

    // Add network devices to location's VPN network
    let network_devices = vec![
        (
            network_device_1.id,
            IpAddr::V6(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0x0100, 1)),
        ),
        (
            network_device_2.id,
            IpAddr::V6(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0x0100, 2)),
        ),
        (
            network_device_3.id,
            IpAddr::V6(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0x0100, 3)),
        ),
    ];

    for (device_id, ip) in network_devices {
        let network_device = WireguardNetworkDevice {
            device_id,
            wireguard_network_id: location.id,
            wireguard_ips: vec![ip],
            preshared_key: None,
            is_authorized: true,
            authorized_at: None,
        };
        network_device.insert(&pool).await.unwrap();
    }

    // Create first ACL rule - Web access
    let acl_rule_1 = AclRule {
        id: NoId,
        name: "Web Access".into(),
        all_networks: false,
        expires: None,
        allow_all_users: false,
        deny_all_users: false,
        allow_all_network_devices: false,
        deny_all_network_devices: false,
        destination: vec!["fc00::0/112".parse().unwrap()],
        ports: vec![
            PortRange::new(80, 80).into(),
            PortRange::new(443, 443).into(),
        ],
        protocols: vec![Protocol::Tcp.into()],
        enabled: true,
        parent_id: None,
        state: RuleState::Applied,
    };
    let locations = vec![location.id];
    let allowed_users = vec![user_1.id, user_2.id]; // First two users can access web
    let denied_users = vec![user_3.id]; // Third user explicitly denied
    let allowed_groups = vec![group_1.id]; // First group allowed
    let denied_groups = Vec::new();
    let allowed_devices = vec![network_device_1.id];
    let denied_devices = vec![network_device_2.id, network_device_3.id];
    let destination_ranges = Vec::new();
    let aliases = Vec::new();

    let _acl_rule_1 = create_acl_rule(
        &pool,
        acl_rule_1,
        locations,
        allowed_users,
        denied_users,
        allowed_groups,
        denied_groups,
        allowed_devices,
        denied_devices,
        destination_ranges,
        aliases,
    )
    .await;

    // Create second ACL rule - DNS access
    let acl_rule_2 = AclRule {
        id: NoId,
        name: "DNS Access".into(),
        all_networks: false,
        expires: None,
        allow_all_users: true, // Allow all users
        deny_all_users: false,
        allow_all_network_devices: false,
        deny_all_network_devices: false,
        destination: Vec::new(), // Will use destination ranges instead
        ports: vec![PortRange::new(53, 53).into()],
        protocols: vec![Protocol::Udp.into(), Protocol::Tcp.into()],
        enabled: true,
        parent_id: None,
        state: RuleState::Applied,
    };
    let locations_2 = vec![location.id];
    let allowed_users_2 = Vec::new();
    let denied_users_2 = vec![user_5.id]; // Fifth user denied DNS
    let allowed_groups_2 = Vec::new();
    let denied_groups_2 = vec![group_2.id];
    let allowed_devices_2 = vec![network_device_1.id, network_device_2.id]; // First two network devices allowed
    let denied_devices_2 = vec![network_device_3.id]; // Third network device denied
    let destination_ranges_2 = vec![
        ("fc00::1:13".parse().unwrap(), "fc00::1:43".parse().unwrap()),
        ("fc00::1:52".parse().unwrap(), "fc00::2:43".parse().unwrap()),
    ];
    let aliases_2 = Vec::new();

    let _acl_rule_2 = create_acl_rule(
        &pool,
        acl_rule_2,
        locations_2,
        allowed_users_2,
        denied_users_2,
        allowed_groups_2,
        denied_groups_2,
        allowed_devices_2,
        denied_devices_2,
        destination_ranges_2,
        aliases_2,
    )
    .await;

    let mut conn = pool.acquire().await.unwrap();

    // try to generate firewall config with ACL disabled
    location.acl_enabled = false;
    let generated_firewall_config = location.try_get_firewall_config(&mut conn).await.unwrap();
    assert!(generated_firewall_config.is_none());

    // generate firewall config with default policy Allow
    location.acl_enabled = true;
    location.acl_default_allow = true;
    let generated_firewall_config = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        generated_firewall_config.default_policy,
        i32::from(FirewallPolicy::Allow)
    );

    let generated_firewall_rules = generated_firewall_config.rules;

    assert_eq!(generated_firewall_rules.len(), 4);

    // First ACL - Web Access ALLOW
    let web_allow_rule = &generated_firewall_rules[0];
    assert_eq!(web_allow_rule.verdict, i32::from(FirewallPolicy::Allow));
    assert_eq!(web_allow_rule.protocols, vec![i32::from(Protocol::Tcp)]);
    assert_eq!(
        web_allow_rule.destination_addrs,
        [IpAddress {
            address: Some(Address::IpSubnet("fc00::/112".to_string())),
        }]
    );
    assert_eq!(
        web_allow_rule.destination_ports,
        [
            Port {
                port: Some(PortInner::SinglePort(80))
            },
            Port {
                port: Some(PortInner::SinglePort(443))
            }
        ]
    );
    // Source addresses should include devices of users 1,2 and network_device_1
    assert_eq!(
        web_allow_rule.source_addrs,
        [
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::1:1".to_string(),
                    end: "ff00::1:2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::2:1".to_string(),
                    end: "ff00::2:2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::Ip("ff00::100:1".to_string())),
            },
        ]
    );

    // First ACL - Web Access DENY
    let web_deny_rule = &generated_firewall_rules[2];
    assert_eq!(web_deny_rule.verdict, i32::from(FirewallPolicy::Deny));
    assert!(web_deny_rule.protocols.is_empty());
    assert!(web_deny_rule.destination_ports.is_empty());
    assert!(web_deny_rule.source_addrs.is_empty());
    assert_eq!(
        web_deny_rule.destination_addrs,
        [IpAddress {
            address: Some(Address::IpSubnet("fc00::/112".to_string())),
        }]
    );

    // Second ACL - DNS Access ALLOW
    let dns_allow_rule = &generated_firewall_rules[1];
    assert_eq!(dns_allow_rule.verdict, i32::from(FirewallPolicy::Allow));
    assert_eq!(
        dns_allow_rule.protocols,
        [i32::from(Protocol::Tcp), i32::from(Protocol::Udp)]
    );
    assert_eq!(
        dns_allow_rule.destination_ports,
        [Port {
            port: Some(PortInner::SinglePort(53))
        }]
    );

    let expected_destination_addrs = vec![
        IpAddress {
            address: Some(Address::Ip("fc00::1:13".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:14/126".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:18/125".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:20/123".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:40/126".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:52/127".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:54/126".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:58/125".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:60/123".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:80/121".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:100/120".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:200/119".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:400/118".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:800/117".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:1000/116".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:2000/115".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:4000/114".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:8000/113".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::2:0/122".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::2:40/126".to_string())),
        },
    ];

    // Source addresses should include network_devices 1,2
    assert_eq!(
        dns_allow_rule.source_addrs,
        [
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::1:1".to_string(),
                    end: "ff00::1:2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::2:1".to_string(),
                    end: "ff00::2:2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::100:1".to_string(),
                    end: "ff00::100:2".to_string(),
                })),
            },
        ]
    );
    assert_eq!(dns_allow_rule.destination_addrs, expected_destination_addrs);

    // Second ACL - DNS Access DENY
    let dns_deny_rule = &generated_firewall_rules[3];
    assert_eq!(dns_deny_rule.verdict, i32::from(FirewallPolicy::Deny));
    assert!(dns_deny_rule.protocols.is_empty(),);
    assert!(dns_deny_rule.destination_ports.is_empty(),);
    assert!(dns_deny_rule.source_addrs.is_empty(),);
    assert_eq!(dns_deny_rule.destination_addrs, expected_destination_addrs);
}

#[sqlx::test]
async fn test_generate_firewall_rules_ipv4_and_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;

    let mut rng = thread_rng();

    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: false,
        address: vec![
            IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).unwrap(),
            IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap(),
        ],
        ..Default::default()
    };
    let mut location = location.save(&pool).await.unwrap();

    // Setup test users and their devices
    let user_1: User<NoId> = rng.gen();
    let user_1 = user_1.save(&pool).await.unwrap();
    let user_2: User<NoId> = rng.gen();
    let user_2 = user_2.save(&pool).await.unwrap();
    let user_3: User<NoId> = rng.gen();
    let user_3 = user_3.save(&pool).await.unwrap();
    let user_4: User<NoId> = rng.gen();
    let user_4 = user_4.save(&pool).await.unwrap();
    let user_5: User<NoId> = rng.gen();
    let user_5 = user_5.save(&pool).await.unwrap();

    for user in [&user_1, &user_2, &user_3, &user_4, &user_5] {
        // Create 2 devices per user
        for device_num in 1..3 {
            let device = Device {
                id: NoId,
                name: format!("device-{}-{}", user.id, device_num),
                user_id: user.id,
                device_type: DeviceType::User,
                description: None,
                wireguard_pubkey: Default::default(),
                created: Default::default(),
                configured: true,
            };
            let device = device.save(&pool).await.unwrap();

            // Add device to location's VPN network
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location.id,
                wireguard_ips: vec![
                    IpAddr::V4(Ipv4Addr::new(10, 0, user.id as u8, device_num as u8)),
                    IpAddr::V6(Ipv6Addr::new(
                        0xff00,
                        0,
                        0,
                        0,
                        0,
                        0,
                        user.id as u16,
                        device_num as u16,
                    )),
                ],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
        }
    }

    // Setup test groups
    let group_1 = Group {
        id: NoId,
        name: "group_1".into(),
        ..Default::default()
    };
    let group_1 = group_1.save(&pool).await.unwrap();
    let group_2 = Group {
        id: NoId,
        name: "group_2".into(),
        ..Default::default()
    };
    let group_2 = group_2.save(&pool).await.unwrap();

    // Assign users to groups:
    // Group 1: users 1,2
    // Group 2: users 3,4
    let group_assignments = vec![
        (&group_1, vec![&user_1, &user_2]),
        (&group_2, vec![&user_3, &user_4]),
    ];

    for (group, users) in group_assignments {
        for user in users {
            query!(
                "INSERT INTO group_user (user_id, group_id) VALUES ($1, $2)",
                user.id,
                group.id
            )
            .execute(&pool)
            .await
            .unwrap();
        }
    }

    // Create some network devices
    let network_device_1 = Device {
        id: NoId,
        name: "network-device-1".into(),
        user_id: user_1.id, // Owned by user 1
        device_type: DeviceType::Network,
        description: Some("Test network device 1".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_1 = network_device_1.save(&pool).await.unwrap();

    let network_device_2 = Device {
        id: NoId,
        name: "network-device-2".into(),
        user_id: user_2.id, // Owned by user 2
        device_type: DeviceType::Network,
        description: Some("Test network device 2".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_2 = network_device_2.save(&pool).await.unwrap();

    let network_device_3 = Device {
        id: NoId,
        name: "network-device-3".into(),
        user_id: user_3.id, // Owned by user 3
        device_type: DeviceType::Network,
        description: Some("Test network device 3".into()),
        wireguard_pubkey: Default::default(),
        created: Default::default(),
        configured: true,
    };
    let network_device_3 = network_device_3.save(&pool).await.unwrap();

    // Add network devices to location's VPN network
    let network_devices = vec![
        (
            network_device_1.id,
            vec![
                IpAddr::V4(Ipv4Addr::new(10, 0, 100, 1)),
                IpAddr::V6(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0x0100, 1)),
            ],
        ),
        (
            network_device_2.id,
            vec![
                IpAddr::V4(Ipv4Addr::new(10, 0, 100, 2)),
                IpAddr::V6(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0x0100, 2)),
            ],
        ),
        (
            network_device_3.id,
            vec![
                IpAddr::V4(Ipv4Addr::new(10, 0, 100, 3)),
                IpAddr::V6(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0x0100, 3)),
            ],
        ),
    ];

    for (device_id, ips) in network_devices {
        let network_device = WireguardNetworkDevice {
            device_id,
            wireguard_network_id: location.id,
            wireguard_ips: ips,
            preshared_key: None,
            is_authorized: true,
            authorized_at: None,
        };
        network_device.insert(&pool).await.unwrap();
    }

    // Create first ACL rule - Web access
    let acl_rule_1 = AclRule {
        id: NoId,
        name: "Web Access".into(),
        all_networks: false,
        expires: None,
        allow_all_users: false,
        deny_all_users: false,
        allow_all_network_devices: false,
        deny_all_network_devices: false,
        destination: vec![
            "192.168.1.0/24".parse().unwrap(),
            "fc00::0/112".parse().unwrap(),
        ],
        ports: vec![
            PortRange::new(80, 80).into(),
            PortRange::new(443, 443).into(),
        ],
        protocols: vec![Protocol::Tcp.into()],
        enabled: true,
        parent_id: None,
        state: RuleState::Applied,
    };
    let locations = vec![location.id];
    let allowed_users = vec![user_1.id, user_2.id]; // First two users can access web
    let denied_users = vec![user_3.id]; // Third user explicitly denied
    let allowed_groups = vec![group_1.id]; // First group allowed
    let denied_groups = Vec::new();
    let allowed_devices = vec![network_device_1.id];
    let denied_devices = vec![network_device_2.id, network_device_3.id];
    let destination_ranges = Vec::new();
    let aliases = Vec::new();

    let _acl_rule_1 = create_acl_rule(
        &pool,
        acl_rule_1,
        locations,
        allowed_users,
        denied_users,
        allowed_groups,
        denied_groups,
        allowed_devices,
        denied_devices,
        destination_ranges,
        aliases,
    )
    .await;

    // Create second ACL rule - DNS access
    let acl_rule_2 = AclRule {
        id: NoId,
        name: "DNS Access".into(),
        all_networks: false,
        expires: None,
        allow_all_users: true, // Allow all users
        deny_all_users: false,
        allow_all_network_devices: false,
        deny_all_network_devices: false,
        destination: Vec::new(), // Will use destination ranges instead
        ports: vec![PortRange::new(53, 53).into()],
        protocols: vec![Protocol::Udp.into(), Protocol::Tcp.into()],
        enabled: true,
        parent_id: None,
        state: RuleState::Applied,
    };
    let locations_2 = vec![location.id];
    let allowed_users_2 = Vec::new();
    let denied_users_2 = vec![user_5.id]; // Fifth user denied DNS
    let allowed_groups_2 = Vec::new();
    let denied_groups_2 = vec![group_2.id];
    let allowed_devices_2 = vec![network_device_1.id, network_device_2.id]; // First two network devices allowed
    let denied_devices_2 = vec![network_device_3.id]; // Third network device denied
    let destination_ranges_2 = vec![
        ("10.0.1.13".parse().unwrap(), "10.0.1.43".parse().unwrap()),
        ("10.0.1.52".parse().unwrap(), "10.0.2.43".parse().unwrap()),
        ("fc00::1:13".parse().unwrap(), "fc00::1:43".parse().unwrap()),
        ("fc00::1:52".parse().unwrap(), "fc00::2:43".parse().unwrap()),
    ];
    let aliases_2 = Vec::new();

    let _acl_rule_2 = create_acl_rule(
        &pool,
        acl_rule_2,
        locations_2,
        allowed_users_2,
        denied_users_2,
        allowed_groups_2,
        denied_groups_2,
        allowed_devices_2,
        denied_devices_2,
        destination_ranges_2,
        aliases_2,
    )
    .await;

    let mut conn = pool.acquire().await.unwrap();

    // try to generate firewall config with ACL disabled
    location.acl_enabled = false;
    let generated_firewall_config = location.try_get_firewall_config(&mut conn).await.unwrap();
    assert!(generated_firewall_config.is_none());

    // generate firewall config with default policy Allow
    location.acl_enabled = true;
    location.acl_default_allow = true;
    let generated_firewall_config = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        generated_firewall_config.default_policy,
        i32::from(FirewallPolicy::Allow)
    );

    let generated_firewall_rules = generated_firewall_config.rules;

    assert_eq!(generated_firewall_rules.len(), 8);

    // First ACL - Web Access ALLOW
    let web_allow_rule_ipv4 = &generated_firewall_rules[0];
    assert_eq!(
        web_allow_rule_ipv4.verdict,
        i32::from(FirewallPolicy::Allow)
    );
    assert_eq!(
        web_allow_rule_ipv4.protocols,
        vec![i32::from(Protocol::Tcp)]
    );
    assert_eq!(
        web_allow_rule_ipv4.destination_addrs,
        vec![IpAddress {
            address: Some(Address::IpSubnet("192.168.1.0/24".to_string())),
        }]
    );
    assert_eq!(
        web_allow_rule_ipv4.destination_ports,
        vec![
            Port {
                port: Some(PortInner::SinglePort(80))
            },
            Port {
                port: Some(PortInner::SinglePort(443))
            }
        ]
    );
    // Source addresses should include devices of users 1,2 and network_device_1
    assert_eq!(
        web_allow_rule_ipv4.source_addrs,
        vec![
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.1.1".to_string(),
                    end: "10.0.1.2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.2.1".to_string(),
                    end: "10.0.2.2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::Ip("10.0.100.1".to_string())),
            },
        ]
    );

    let web_allow_rule_ipv6 = &generated_firewall_rules[1];
    assert_eq!(
        web_allow_rule_ipv6.verdict,
        i32::from(FirewallPolicy::Allow)
    );
    assert_eq!(web_allow_rule_ipv6.protocols, [i32::from(Protocol::Tcp)]);
    assert_eq!(
        web_allow_rule_ipv6.destination_addrs,
        [IpAddress {
            address: Some(Address::IpSubnet("fc00::/112".to_string())),
        }]
    );
    assert_eq!(
        web_allow_rule_ipv6.destination_ports,
        [
            Port {
                port: Some(PortInner::SinglePort(80))
            },
            Port {
                port: Some(PortInner::SinglePort(443))
            }
        ]
    );
    // Source addresses should include devices of users 1,2 and network_device_1
    assert_eq!(
        web_allow_rule_ipv6.source_addrs,
        [
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::1:1".to_string(),
                    end: "ff00::1:2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::2:1".to_string(),
                    end: "ff00::2:2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::Ip("ff00::100:1".to_string())),
            },
        ]
    );

    // First ACL - Web Access DENY
    let web_deny_rule_ipv4 = &generated_firewall_rules[4];
    assert_eq!(web_deny_rule_ipv4.verdict, i32::from(FirewallPolicy::Deny));
    assert!(web_deny_rule_ipv4.protocols.is_empty());
    assert!(web_deny_rule_ipv4.destination_ports.is_empty());
    assert!(web_deny_rule_ipv4.source_addrs.is_empty());
    assert_eq!(
        web_deny_rule_ipv4.destination_addrs,
        [IpAddress {
            address: Some(Address::IpSubnet("192.168.1.0/24".to_string())),
        }]
    );

    let web_deny_rule_ipv6 = &generated_firewall_rules[5];
    assert_eq!(web_deny_rule_ipv6.verdict, i32::from(FirewallPolicy::Deny));
    assert!(web_deny_rule_ipv6.protocols.is_empty());
    assert!(web_deny_rule_ipv6.destination_ports.is_empty());
    assert!(web_deny_rule_ipv6.source_addrs.is_empty());
    assert_eq!(
        web_deny_rule_ipv6.destination_addrs,
        [IpAddress {
            address: Some(Address::IpSubnet("fc00::/112".to_string())),
        }]
    );

    // Second ACL - DNS Access ALLOW
    let dns_allow_rule_ipv4 = &generated_firewall_rules[2];
    assert_eq!(
        dns_allow_rule_ipv4.verdict,
        i32::from(FirewallPolicy::Allow)
    );
    assert_eq!(
        dns_allow_rule_ipv4.protocols,
        [i32::from(Protocol::Tcp), i32::from(Protocol::Udp)]
    );
    assert_eq!(
        dns_allow_rule_ipv4.destination_ports,
        [Port {
            port: Some(PortInner::SinglePort(53))
        }]
    );
    // Source addresses should include network_devices 1,2
    assert_eq!(
        dns_allow_rule_ipv4.source_addrs,
        [
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.1.1".to_string(),
                    end: "10.0.1.2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.2.1".to_string(),
                    end: "10.0.2.2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "10.0.100.1".to_string(),
                    end: "10.0.100.2".to_string(),
                })),
            },
        ]
    );

    let expected_destination_addrs_v4 = vec![
        IpAddress {
            address: Some(Address::Ip("10.0.1.13".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.14/31".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.16/28".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.32/29".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.40/30".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.52/30".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.56/29".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.64/26".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.1.128/25".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.2.0/27".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.2.32/29".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("10.0.2.40/30".to_string())),
        },
    ];

    assert_eq!(
        dns_allow_rule_ipv4.destination_addrs,
        expected_destination_addrs_v4
    );

    let dns_allow_rule_ipv6 = &generated_firewall_rules[3];
    assert_eq!(
        dns_allow_rule_ipv6.verdict,
        i32::from(FirewallPolicy::Allow)
    );
    assert_eq!(
        dns_allow_rule_ipv6.protocols,
        [i32::from(Protocol::Tcp), i32::from(Protocol::Udp)]
    );
    assert_eq!(
        dns_allow_rule_ipv6.destination_ports,
        [Port {
            port: Some(PortInner::SinglePort(53))
        }]
    );
    // Source addresses should include network_devices 1,2
    assert_eq!(
        dns_allow_rule_ipv6.source_addrs,
        [
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::1:1".to_string(),
                    end: "ff00::1:2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::2:1".to_string(),
                    end: "ff00::2:2".to_string(),
                })),
            },
            IpAddress {
                address: Some(Address::IpRange(IpRange {
                    start: "ff00::100:1".to_string(),
                    end: "ff00::100:2".to_string(),
                })),
            },
        ]
    );

    let expected_destination_addrs_v6 = vec![
        IpAddress {
            address: Some(Address::Ip("fc00::1:13".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:14/126".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:18/125".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:20/123".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:40/126".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:52/127".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:54/126".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:58/125".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:60/123".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:80/121".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:100/120".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:200/119".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:400/118".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:800/117".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:1000/116".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:2000/115".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:4000/114".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::1:8000/113".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::2:0/122".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("fc00::2:40/126".to_string())),
        },
    ];

    assert_eq!(
        dns_allow_rule_ipv6.destination_addrs,
        expected_destination_addrs_v6
    );

    // Second ACL - DNS Access DENY
    let dns_deny_rule_ipv4 = &generated_firewall_rules[6];
    assert_eq!(dns_deny_rule_ipv4.verdict, i32::from(FirewallPolicy::Deny));
    assert!(dns_deny_rule_ipv4.protocols.is_empty(),);
    assert!(dns_deny_rule_ipv4.destination_ports.is_empty(),);
    assert!(dns_deny_rule_ipv4.source_addrs.is_empty(),);
    assert_eq!(
        dns_deny_rule_ipv4.destination_addrs,
        expected_destination_addrs_v4
    );

    let dns_deny_rule_ipv6 = &generated_firewall_rules[7];
    assert_eq!(dns_deny_rule_ipv6.verdict, i32::from(FirewallPolicy::Deny));
    assert!(dns_deny_rule_ipv6.protocols.is_empty(),);
    assert!(dns_deny_rule_ipv6.destination_ports.is_empty(),);
    assert!(dns_deny_rule_ipv6.source_addrs.is_empty(),);
    assert_eq!(
        dns_deny_rule_ipv6.destination_addrs,
        expected_destination_addrs_v6
    );
}

#[sqlx::test]
async fn test_expired_acl_rules_ipv4(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create expired ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: Some(DateTime::UNIX_EPOCH.naive_utc()),
        enabled: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: Some(DateTime::UNIX_EPOCH.naive_utc()),
        enabled: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were expired
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules not expired
    acl_rule_1.expires = None;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.expires = Some(NaiveDateTime::MAX);
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 2);
}

#[sqlx::test]
async fn test_expired_acl_rules_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap()],
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create expired ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: Some(DateTime::UNIX_EPOCH.naive_utc()),
        enabled: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: Some(DateTime::UNIX_EPOCH.naive_utc()),
        enabled: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were expired
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules not expired
    acl_rule_1.expires = None;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.expires = Some(NaiveDateTime::MAX);
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 2);
}

#[sqlx::test]
async fn test_expired_acl_rules_ipv4_and_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![
            IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).unwrap(),
            IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap(),
        ],
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create expired ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: Some(DateTime::UNIX_EPOCH.naive_utc()),
        enabled: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: Some(DateTime::UNIX_EPOCH.naive_utc()),
        enabled: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were expired
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules not expired
    acl_rule_1.expires = None;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.expires = Some(NaiveDateTime::MAX);
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 4);
}

#[sqlx::test]
async fn test_disabled_acl_rules_ipv4(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create disabled ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: false,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: false,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were disabled
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules enabled
    acl_rule_1.enabled = true;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.enabled = true;
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 2);
}

#[sqlx::test]
async fn test_disabled_acl_rules_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap()],
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create disabled ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: false,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: false,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were disabled
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules enabled
    acl_rule_1.enabled = true;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.enabled = true;
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 2);
}

#[sqlx::test]
async fn test_disabled_acl_rules_ipv4_and_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![
            IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).unwrap(),
            IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap(),
        ],
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create disabled ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: false,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: false,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were disabled
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules enabled
    acl_rule_1.enabled = true;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.enabled = true;
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 4);
}

#[sqlx::test]
async fn test_unapplied_acl_rules_ipv4(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create unapplied ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::New,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::Modified,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were not applied
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules applied
    acl_rule_1.state = RuleState::Applied;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.state = RuleState::Applied;
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 2);
}

#[sqlx::test]
async fn test_unapplied_acl_rules_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap()],
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create unapplied ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::New,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::Modified,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were not applied
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules applied
    acl_rule_1.state = RuleState::Applied;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.state = RuleState::Applied;
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 2);
}

#[sqlx::test]
async fn test_unapplied_acl_rules_ipv4_and_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![
            IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).unwrap(),
            IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap(),
        ],
        ..Default::default()
    };
    let location = location.save(&pool).await.unwrap();

    // create unapplied ACL rules
    let mut acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::New,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let mut acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::Modified,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to location
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were not applied
    assert_eq!(generated_firewall_rules.len(), 0);

    // make both rules applied
    acl_rule_1.state = RuleState::Applied;
    acl_rule_1.save(&pool).await.unwrap();

    acl_rule_2.state = RuleState::Applied;
    acl_rule_2.save(&pool).await.unwrap();

    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;
    assert_eq!(generated_firewall_rules.len(), 4);
}

#[sqlx::test]
async fn test_acl_rules_all_locations_ipv4(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    let mut rng = thread_rng();

    // Create test location
    let location_1 = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        ..Default::default()
    };
    let location_1 = location_1.save(&pool).await.unwrap();

    // Create another test location
    let location_2 = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        ..Default::default()
    };
    let location_2 = location_2.save(&pool).await.unwrap();
    // Setup some test users and their devices
    let user_1: User<NoId> = rng.gen();
    let user_1 = user_1.save(&pool).await.unwrap();
    let user_2: User<NoId> = rng.gen();
    let user_2 = user_2.save(&pool).await.unwrap();

    for user in [&user_1, &user_2] {
        // Create 2 devices per user
        for device_num in 1..3 {
            let device = Device {
                id: NoId,
                name: format!("device-{}-{}", user.id, device_num),
                user_id: user.id,
                device_type: DeviceType::User,
                description: None,
                wireguard_pubkey: Default::default(),
                created: Default::default(),
                configured: true,
            };
            let device = device.save(&pool).await.unwrap();

            // Add device to location's VPN network
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location_1.id,
                wireguard_ips: vec![IpAddr::V4(Ipv4Addr::new(
                    10,
                    0,
                    user.id as u8,
                    device_num as u8,
                ))],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location_2.id,
                wireguard_ips: vec![IpAddr::V4(Ipv4Addr::new(
                    10,
                    10,
                    user.id as u8,
                    device_num as u8,
                ))],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
        }
    }

    // create ACL rules
    let acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::Applied,
        destination: vec!["192.168.1.0/24".parse().unwrap()],
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    let acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        all_networks: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    let _acl_rule_3 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        all_networks: true,
        allow_all_users: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to locations
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location_1.id,
        };
        obj.save(&pool).await.unwrap();
    }
    for rule in [&acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location_2.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location_1
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were assigned to this location
    assert_eq!(generated_firewall_rules.len(), 4);

    let generated_firewall_rules = location_2
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // rule with `all_networks` enabled was used for this location
    assert_eq!(generated_firewall_rules.len(), 3);
}

#[sqlx::test]
async fn test_acl_rules_all_locations_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    let mut rng = thread_rng();

    // Create test location
    let location_1 = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap()],
        ..Default::default()
    };
    let location_1 = location_1.save(&pool).await.unwrap();

    // Create another test location
    let location_2 = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap()],
        ..Default::default()
    };
    let location_2 = location_2.save(&pool).await.unwrap();

    // Setup some test users and their devices
    let user_1: User<NoId> = rng.gen();
    let user_1 = user_1.save(&pool).await.unwrap();
    let user_2: User<NoId> = rng.gen();
    let user_2 = user_2.save(&pool).await.unwrap();

    for user in [&user_1, &user_2] {
        // Create 2 devices per user
        for device_num in 1..3 {
            let device = Device {
                id: NoId,
                name: format!("device-{}-{}", user.id, device_num),
                user_id: user.id,
                device_type: DeviceType::User,
                description: None,
                wireguard_pubkey: Default::default(),
                created: Default::default(),
                configured: true,
            };
            let device = device.save(&pool).await.unwrap();

            // Add device to location's VPN network
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location_1.id,
                wireguard_ips: vec![IpAddr::V6(Ipv6Addr::new(
                    0xff00,
                    0,
                    0,
                    0,
                    0,
                    0,
                    user.id as u16,
                    device_num as u16,
                ))],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location_2.id,
                wireguard_ips: vec![IpAddr::V6(Ipv6Addr::new(
                    0xff00,
                    0,
                    0,
                    0,
                    10,
                    10,
                    user.id as u16,
                    device_num as u16,
                ))],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
        }
    }

    // create ACL rules
    let acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::Applied,
        destination: vec!["fc00::0/112".parse().unwrap()],
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    let acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        all_networks: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    let _acl_rule_3 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        all_networks: true,
        allow_all_users: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to locations
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location_1.id,
        };
        obj.save(&pool).await.unwrap();
    }
    for rule in [&acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location_2.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location_1
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were assigned to this location
    assert_eq!(generated_firewall_rules.len(), 4);

    let generated_firewall_rules = location_2
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // rule with `all_networks` enabled was used for this location
    assert_eq!(generated_firewall_rules.len(), 3);
}

#[sqlx::test]
async fn test_acl_rules_all_locations_ipv4_and_ipv6(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;
    let mut rng = thread_rng();

    // Create test location
    let location_1 = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![
            IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).unwrap(),
            IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap(),
        ],
        ..Default::default()
    };
    let location_1 = location_1.save(&pool).await.unwrap();

    // Create another test location
    let location_2 = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        address: vec![
            IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).unwrap(),
            IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).unwrap(),
        ],
        ..Default::default()
    };
    let location_2 = location_2.save(&pool).await.unwrap();
    // Setup some test users and their devices
    let user_1: User<NoId> = rng.gen();
    let user_1 = user_1.save(&pool).await.unwrap();
    let user_2: User<NoId> = rng.gen();
    let user_2 = user_2.save(&pool).await.unwrap();

    for user in [&user_1, &user_2] {
        // Create 2 devices per user
        for device_num in 1..3 {
            let device = Device {
                id: NoId,
                name: format!("device-{}-{}", user.id, device_num),
                user_id: user.id,
                device_type: DeviceType::User,
                description: None,
                wireguard_pubkey: Default::default(),
                created: Default::default(),
                configured: true,
            };
            let device = device.save(&pool).await.unwrap();

            // Add device to location's VPN network
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location_1.id,
                wireguard_ips: vec![
                    IpAddr::V4(Ipv4Addr::new(10, 0, user.id as u8, device_num as u8)),
                    IpAddr::V6(Ipv6Addr::new(
                        0xff00,
                        0,
                        0,
                        0,
                        0,
                        0,
                        user.id as u16,
                        device_num as u16,
                    )),
                ],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location_2.id,
                wireguard_ips: vec![
                    IpAddr::V4(Ipv4Addr::new(10, 10, user.id as u8, device_num as u8)),
                    IpAddr::V6(Ipv6Addr::new(
                        0xff00,
                        0,
                        0,
                        0,
                        10,
                        10,
                        user.id as u16,
                        device_num as u16,
                    )),
                ],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
        }
    }

    // create ACL rules
    let acl_rule_1 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        state: RuleState::Applied,
        destination: vec![
            "192.168.1.0/24".parse().unwrap(),
            "fc00::0/112".parse().unwrap(),
        ],
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    let acl_rule_2 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        all_networks: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    let _acl_rule_3 = AclRule {
        id: NoId,
        expires: None,
        enabled: true,
        all_networks: true,
        allow_all_users: true,
        state: RuleState::Applied,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // assign rules to locations
    for rule in [&acl_rule_1, &acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location_1.id,
        };
        obj.save(&pool).await.unwrap();
    }
    for rule in [&acl_rule_2] {
        let obj = AclRuleNetwork {
            id: NoId,
            rule_id: rule.id,
            network_id: location_2.id,
        };
        obj.save(&pool).await.unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location_1
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // both rules were assigned to this location
    assert_eq!(generated_firewall_rules.len(), 8);

    let generated_firewall_rules = location_2
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // rule with `all_networks` enabled was used for this location
    assert_eq!(generated_firewall_rules.len(), 6);
}

#[sqlx::test]
async fn test_alias_kinds(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;

    let mut rng = thread_rng();

    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // Setup some test users and their devices
    let user_1: User<NoId> = rng.gen();
    let user_1 = user_1.save(&pool).await.unwrap();
    let user_2: User<NoId> = rng.gen();
    let user_2 = user_2.save(&pool).await.unwrap();

    for user in [&user_1, &user_2] {
        // Create 2 devices per user
        for device_num in 1..3 {
            let device = Device {
                id: NoId,
                name: format!("device-{}-{}", user.id, device_num),
                user_id: user.id,
                device_type: DeviceType::User,
                description: None,
                wireguard_pubkey: Default::default(),
                created: Default::default(),
                configured: true,
            };
            let device = device.save(&pool).await.unwrap();

            // Add device to location's VPN network
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location.id,
                wireguard_ips: vec![IpAddr::V4(Ipv4Addr::new(
                    10,
                    0,
                    user.id as u8,
                    device_num as u8,
                ))],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
        }
    }

    // create ACL rule
    let acl_rule = AclRule {
        id: NoId,
        name: "test rule".to_string(),
        expires: None,
        enabled: true,
        state: RuleState::Applied,
        destination: vec!["192.168.1.0/24".parse().unwrap()],
        allow_all_users: true,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // create different kinds of aliases and add them to the rule
    let destination_alias = AclAlias {
        id: NoId,
        name: "destination alias".to_string(),
        kind: AliasKind::Destination,
        ports: vec![PortRange::new(100, 200).into()],
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let component_alias = AclAlias {
        id: NoId,
        kind: AliasKind::Component,
        destination: vec!["10.0.2.3".parse().unwrap()],
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    for alias in [&destination_alias, &component_alias] {
        let obj = AclRuleAlias {
            id: NoId,
            rule_id: acl_rule.id,
            alias_id: alias.id,
        };
        obj.save(&pool).await.unwrap();
    }

    // assign rule to location
    let obj = AclRuleNetwork {
        id: NoId,
        rule_id: acl_rule.id,
        network_id: location.id,
    };
    obj.save(&pool).await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // check generated rules
    assert_eq!(generated_firewall_rules.len(), 4);
    let expected_source_addrs = [
        IpAddress {
            address: Some(Address::IpRange(IpRange {
                start: "10.0.1.1".to_string(),
                end: "10.0.1.2".to_string(),
            })),
        },
        IpAddress {
            address: Some(Address::IpRange(IpRange {
                start: "10.0.2.1".to_string(),
                end: "10.0.2.2".to_string(),
            })),
        },
    ];
    let expected_destination_addrs = [
        IpAddress {
            address: Some(Address::Ip("10.0.2.3".to_string())),
        },
        IpAddress {
            address: Some(Address::IpSubnet("192.168.1.0/24".to_string())),
        },
    ];

    let allow_rule = &generated_firewall_rules[0];
    assert_eq!(allow_rule.verdict, i32::from(FirewallPolicy::Allow));
    assert_eq!(allow_rule.source_addrs, expected_source_addrs);
    assert_eq!(allow_rule.destination_addrs, expected_destination_addrs);
    assert!(allow_rule.destination_ports.is_empty());
    assert!(allow_rule.protocols.is_empty());
    assert_eq!(
        allow_rule.comment,
        Some("ACL 1 - test rule ALLOW".to_string())
    );

    let alias_allow_rule = &generated_firewall_rules[1];
    assert_eq!(alias_allow_rule.verdict, i32::from(FirewallPolicy::Allow));
    assert_eq!(alias_allow_rule.source_addrs, expected_source_addrs);
    assert!(alias_allow_rule.destination_addrs.is_empty());
    assert_eq!(
        alias_allow_rule.destination_ports,
        vec![Port {
            port: Some(PortInner::PortRange(PortRangeProto {
                start: 100,
                end: 200,
            }))
        }]
    );
    assert!(alias_allow_rule.protocols.is_empty());
    assert_eq!(
        alias_allow_rule.comment,
        Some("ACL 1 - test rule, ALIAS 1 - destination alias ALLOW".to_string())
    );

    let deny_rule = &generated_firewall_rules[2];
    assert_eq!(deny_rule.verdict, i32::from(FirewallPolicy::Deny));
    assert!(deny_rule.source_addrs.is_empty());
    assert_eq!(deny_rule.destination_addrs, expected_destination_addrs);
    assert!(deny_rule.destination_ports.is_empty());
    assert!(deny_rule.protocols.is_empty());
    assert_eq!(
        deny_rule.comment,
        Some("ACL 1 - test rule DENY".to_string())
    );

    let alias_deny_rule = &generated_firewall_rules[3];
    assert_eq!(alias_deny_rule.verdict, i32::from(FirewallPolicy::Deny));
    assert!(alias_deny_rule.source_addrs.is_empty());
    assert!(alias_deny_rule.destination_addrs.is_empty());
    assert!(alias_deny_rule.destination_ports.is_empty());
    assert!(alias_deny_rule.protocols.is_empty());
    assert_eq!(
        alias_deny_rule.comment,
        Some("ACL 1 - test rule, ALIAS 1 - destination alias DENY".to_string())
    );
}

#[sqlx::test]
async fn test_destination_alias_only_acl(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = setup_pool(options).await;

    let mut rng = thread_rng();

    // Create test location
    let location = WireguardNetwork {
        id: NoId,
        acl_enabled: true,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // Setup some test users and their devices
    let user_1: User<NoId> = rng.gen();
    let user_1 = user_1.save(&pool).await.unwrap();
    let user_2: User<NoId> = rng.gen();
    let user_2 = user_2.save(&pool).await.unwrap();

    for user in [&user_1, &user_2] {
        // Create 2 devices per user
        for device_num in 1..3 {
            let device = Device {
                id: NoId,
                name: format!("device-{}-{device_num}", user.id),
                user_id: user.id,
                device_type: DeviceType::User,
                description: None,
                wireguard_pubkey: Default::default(),
                created: Default::default(),
                configured: true,
            };
            let device = device.save(&pool).await.unwrap();

            // Add device to location's VPN network
            let network_device = WireguardNetworkDevice {
                device_id: device.id,
                wireguard_network_id: location.id,
                wireguard_ips: vec![IpAddr::V4(Ipv4Addr::new(
                    10,
                    0,
                    user.id as u8,
                    device_num as u8,
                ))],
                preshared_key: None,
                is_authorized: true,
                authorized_at: None,
            };
            network_device.insert(&pool).await.unwrap();
        }
    }

    // create ACL rule without manually configured destination
    let acl_rule = AclRule {
        id: NoId,
        name: "test rule".to_string(),
        expires: None,
        enabled: true,
        state: RuleState::Applied,
        destination: Vec::new(),
        allow_all_users: true,
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();

    // create different kinds of aliases and add them to the rule
    let destination_alias_1 = AclAlias {
        id: NoId,
        name: "postgres".to_string(),
        kind: AliasKind::Destination,
        destination: vec!["10.0.2.3".parse().unwrap()],
        ports: vec![PortRange::new(5432, 5432).into()],
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    let destination_alias_2 = AclAlias {
        id: NoId,
        name: "redis".to_string(),
        kind: AliasKind::Destination,
        destination: vec!["10.0.2.4".parse().unwrap()],
        ports: vec![PortRange::new(6379, 6379).into()],
        ..Default::default()
    }
    .save(&pool)
    .await
    .unwrap();
    for alias in [&destination_alias_1, &destination_alias_2] {
        let obj = AclRuleAlias {
            id: NoId,
            rule_id: acl_rule.id,
            alias_id: alias.id,
        };
        obj.save(&pool).await.unwrap();
    }

    // assign rule to location
    let obj = AclRuleNetwork {
        id: NoId,
        rule_id: acl_rule.id,
        network_id: location.id,
    };
    obj.save(&pool).await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let generated_firewall_rules = location
        .try_get_firewall_config(&mut conn)
        .await
        .unwrap()
        .unwrap()
        .rules;

    // check generated rules
    assert_eq!(generated_firewall_rules.len(), 4);
    let expected_source_addrs = vec![
        IpAddress {
            address: Some(Address::IpRange(IpRange {
                start: "10.0.1.1".to_string(),
                end: "10.0.1.2".to_string(),
            })),
        },
        IpAddress {
            address: Some(Address::IpRange(IpRange {
                start: "10.0.2.1".to_string(),
                end: "10.0.2.2".to_string(),
            })),
        },
    ];

    let alias_allow_rule_1 = &generated_firewall_rules[0];
    assert_eq!(alias_allow_rule_1.verdict, i32::from(FirewallPolicy::Allow));
    assert_eq!(alias_allow_rule_1.source_addrs, expected_source_addrs);
    assert_eq!(
        alias_allow_rule_1.destination_addrs,
        vec![IpAddress {
            address: Some(Address::Ip("10.0.2.3".to_string())),
        },]
    );
    assert_eq!(
        alias_allow_rule_1.destination_ports,
        vec![Port {
            port: Some(PortInner::SinglePort(5432))
        }]
    );
    assert!(alias_allow_rule_1.protocols.is_empty());
    assert_eq!(
        alias_allow_rule_1.comment,
        Some("ACL 1 - test rule, ALIAS 1 - postgres ALLOW".to_string())
    );

    let alias_allow_rule_2 = &generated_firewall_rules[1];
    assert_eq!(alias_allow_rule_2.verdict, i32::from(FirewallPolicy::Allow));
    assert_eq!(alias_allow_rule_2.source_addrs, expected_source_addrs);
    assert_eq!(
        alias_allow_rule_2.destination_addrs,
        vec![IpAddress {
            address: Some(Address::Ip("10.0.2.4".to_string())),
        },]
    );
    assert_eq!(
        alias_allow_rule_2.destination_ports,
        vec![Port {
            port: Some(PortInner::SinglePort(6379))
        }]
    );
    assert!(alias_allow_rule_2.protocols.is_empty());
    assert_eq!(
        alias_allow_rule_2.comment,
        Some("ACL 1 - test rule, ALIAS 2 - redis ALLOW".to_string())
    );

    let alias_deny_rule_1 = &generated_firewall_rules[2];
    assert_eq!(alias_deny_rule_1.verdict, i32::from(FirewallPolicy::Deny));
    assert!(alias_deny_rule_1.source_addrs.is_empty());
    assert_eq!(
        alias_deny_rule_1.destination_addrs,
        vec![IpAddress {
            address: Some(Address::Ip("10.0.2.3".to_string())),
        },]
    );
    assert!(alias_deny_rule_1.destination_ports.is_empty());
    assert!(alias_deny_rule_1.protocols.is_empty());
    assert_eq!(
        alias_deny_rule_1.comment,
        Some("ACL 1 - test rule, ALIAS 1 - postgres DENY".to_string())
    );

    let alias_deny_rule_2 = &generated_firewall_rules[3];
    assert_eq!(alias_deny_rule_2.verdict, i32::from(FirewallPolicy::Deny));
    assert!(alias_deny_rule_2.source_addrs.is_empty());
    assert_eq!(
        alias_deny_rule_2.destination_addrs,
        vec![IpAddress {
            address: Some(Address::Ip("10.0.2.4".to_string())),
        },]
    );
    assert!(alias_deny_rule_2.destination_ports.is_empty());
    assert!(alias_deny_rule_2.protocols.is_empty());
    assert_eq!(
        alias_deny_rule_2.comment,
        Some("ACL 1 - test rule, ALIAS 2 - redis DENY".to_string())
    );
}
