package Proxmox::Sys::Net;

use strict;
use warnings;

use base qw(Exporter);
our @EXPORT_OK = qw(parse_ip_address parse_ip_mask);

my $IPV4OCTET = "(?:25[0-5]|(?:2[0-4]|1[0-9]|[1-9])?[0-9])";
my $IPV4RE = "(?:(?:$IPV4OCTET\\.){3}$IPV4OCTET)";
my $IPV6H16 = "(?:[0-9a-fA-F]{1,4})";
my $IPV6LS32 = "(?:(?:$IPV4RE|$IPV6H16:$IPV6H16))";

my $IPV6RE = "(?:" .
    "(?:(?:" .                             "(?:$IPV6H16:){6})$IPV6LS32)|" .
    "(?:(?:" .                           "::(?:$IPV6H16:){5})$IPV6LS32)|" .
    "(?:(?:(?:" .              "$IPV6H16)?::(?:$IPV6H16:){4})$IPV6LS32)|" .
    "(?:(?:(?:(?:$IPV6H16:){0,1}$IPV6H16)?::(?:$IPV6H16:){3})$IPV6LS32)|" .
    "(?:(?:(?:(?:$IPV6H16:){0,2}$IPV6H16)?::(?:$IPV6H16:){2})$IPV6LS32)|" .
    "(?:(?:(?:(?:$IPV6H16:){0,3}$IPV6H16)?::(?:$IPV6H16:){1})$IPV6LS32)|" .
    "(?:(?:(?:(?:$IPV6H16:){0,4}$IPV6H16)?::" .           ")$IPV6LS32)|" .
    "(?:(?:(?:(?:$IPV6H16:){0,5}$IPV6H16)?::" .            ")$IPV6H16)|" .
    "(?:(?:(?:(?:$IPV6H16:){0,6}$IPV6H16)?::" .                    ")))";

my $IPRE = "(?:$IPV4RE|$IPV6RE)";


my $ipv4_mask_hash = {
    '128.0.0.0' => 1,
    '192.0.0.0' => 2,
    '224.0.0.0' => 3,
    '240.0.0.0' => 4,
    '248.0.0.0' => 5,
    '252.0.0.0' => 6,
    '254.0.0.0' => 7,
    '255.0.0.0' => 8,
    '255.128.0.0' => 9,
    '255.192.0.0' => 10,
    '255.224.0.0' => 11,
    '255.240.0.0' => 12,
    '255.248.0.0' => 13,
    '255.252.0.0' => 14,
    '255.254.0.0' => 15,
    '255.255.0.0' => 16,
    '255.255.128.0' => 17,
    '255.255.192.0' => 18,
    '255.255.224.0' => 19,
    '255.255.240.0' => 20,
    '255.255.248.0' => 21,
    '255.255.252.0' => 22,
    '255.255.254.0' => 23,
    '255.255.255.0' => 24,
    '255.255.255.128' => 25,
    '255.255.255.192' => 26,
    '255.255.255.224' => 27,
    '255.255.255.240' => 28,
    '255.255.255.248' => 29,
    '255.255.255.252' => 30,
    '255.255.255.254' => 31,
    '255.255.255.255' => 32
};

my $ipv4_reverse_mask = [
    '0.0.0.0',
    '128.0.0.0',
    '192.0.0.0',
    '224.0.0.0',
    '240.0.0.0',
    '248.0.0.0',
    '252.0.0.0',
    '254.0.0.0',
    '255.0.0.0',
    '255.128.0.0',
    '255.192.0.0',
    '255.224.0.0',
    '255.240.0.0',
    '255.248.0.0',
    '255.252.0.0',
    '255.254.0.0',
    '255.255.0.0',
    '255.255.128.0',
    '255.255.192.0',
    '255.255.224.0',
    '255.255.240.0',
    '255.255.248.0',
    '255.255.252.0',
    '255.255.254.0',
    '255.255.255.0',
    '255.255.255.128',
    '255.255.255.192',
    '255.255.255.224',
    '255.255.255.240',
    '255.255.255.248',
    '255.255.255.252',
    '255.255.255.254',
    '255.255.255.255',
];

# returns (addr, version) tuple
sub parse_ip_address {
    my ($text) = @_;

    if ($text =~ m!^\s*($IPV4RE)\s*$!) {
	return ($1, 4);
    } elsif ($text =~ m!^\s*($IPV6RE)\s*$!) {
	return ($1, 6);
    }
    return (undef, undef);
}

sub parse_ip_mask {
    my ($text, $ip_version) = @_;
    $text =~ s/^\s+//;
    $text =~ s/\s+$//;
    if ($ip_version == 6 && ($text =~ m/^(\d+)$/) && $1 >= 8 && $1 <= 126) {
	return $text;
    } elsif ($ip_version == 4 && ($text =~ m/^(\d+)$/) && $1 >= 8 && $1 <= 32) {
	return $text;
    } elsif ($ip_version == 4 && defined($ipv4_mask_hash->{$text})) {
	# costs nothing to handle 255.x.y.z style masks, so continue to allow it
	return $ipv4_mask_hash->{$text};
    }
    return;
}

sub get_ip_config {

    my $ifaces = {};
    my $default;

    my $links = `ip -o l`;
    foreach my $l (split /\n/,$links) {
	my ($index, $name, $flags, $state, $mac) = $l =~ m/^(\d+):\s+(\S+):\s+<(\S+)>.*\s+state\s+(\S+)\s+.*\s+link\/ether\s+(\S+)\s+/;
	next if !$name || $name eq 'lo';

	my $driver = readlink "/sys/class/net/$name/device/driver" || 'unknown';
	$driver =~ s!^.*/!!;

	$ifaces->{"$index"} = {
	    name => $name,
	    driver => $driver,
	    flags => $flags,
	    state => $state,
	    mac => $mac,
	};

	my $addresses = `ip -o a s $name`;
	foreach my $a (split /\n/,$addresses) {
	    my ($family, $ip, $prefix) = $a =~ m/^\Q$index\E:\s+\Q$name\E\s+(inet|inet6)\s+($IPRE)\/(\d+)\s+/;
	    next if !$ip;
	    next if $a =~ /scope\s+link/; # ignore link local

	    my $mask = $prefix;

	    if ($family eq 'inet') {
		next if !$ip =~ /$IPV4RE/;
		next if $prefix < 8 || $prefix > 32;
		$mask = @$ipv4_reverse_mask[$prefix];
	    } else {
		next if !$ip =~ /$IPV6RE/;
	    }

	    $default = $index if !$default;

	    $ifaces->{"$index"}->{"$family"} = {
		prefix => $prefix,
		mask => $mask,
		addr => $ip,
	    };
	}
    }


    my $route = `ip route`;
    my ($gateway) = $route =~ m/^default\s+via\s+(\S+)\s+/m;

    my $resolvconf = `cat /etc/resolv.conf`;
    my ($dnsserver) = $resolvconf =~ m/^nameserver\s+(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})$/m;
    my ($domain) = $resolvconf =~ m/^domain\s+(\S+)$/m;

    return {
	default => $default,
	ifaces => $ifaces,
	gateway => $gateway,
	dnsserver => $dnsserver,
	domain => $domain,
    }
}

1;
