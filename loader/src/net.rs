// SPDX-License-Identifier: GPL-3.0-only
//! Ethernet, address configuration, DNS and HTTP.
//!
//! The Seeq 80C03 driver is `irix7-seeq` from IRIX 7. smoltcp supplies ARP,
//! IPv4, TCP, DHCP and DNS. The driver is polled; indyboot takes no
//! interrupts.
//!
//! The driver's descriptors and frame buffers are read and written by the
//! HPC3 DMA engines, so they live at a fixed physical address reached only
//! through kseg1 (uncached). indyboot never touches that range through
//! kseg0, so no cache line can go stale or be written back over a frame.

use crate::clock;
use crate::mem::{Map, DMA_PHYS, DMA_SIZE};
use core::net::Ipv4Addr;
use indyboot_core::bootp;
use indyboot_core::http::{self, HeadError, HEAD_MAX};
use indyboot_core::url::{self, Url, UrlBuf, UrlError};
use irix7_seeq::SeeqDevice;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet, SocketStorage};
use smoltcp::phy::{Device, RxToken, TxToken};
use smoltcp::socket::{dhcpv4, dns, tcp};
use smoltcp::wire::{DnsQueryType, EthernetAddress, HardwareAddress, IpAddress, IpCidr};

const TCP_RX_SIZE: usize = 16384;
const TCP_TX_SIZE: usize = 1024;

const BOOTP_TRIES: u32 = 3;
const BOOTP_WAIT_MS: u64 = 2_000;
const DHCP_WAIT_MS: u64 = 8_000;
const DNS_WAIT_MS: u64 = 10_000;
const CONNECT_WAIT_MS: u64 = 15_000;
/// A transfer fails after this long with no bytes from the server.
const IDLE_WAIT_MS: u64 = 30_000;
const MAX_REDIRECTS: u32 = 4;

static mut SOCKET_STORAGE: [SocketStorage<'static>; 3] = [SocketStorage::EMPTY; 3];
static mut TCP_RX: [u8; TCP_RX_SIZE] = [0; TCP_RX_SIZE];
static mut TCP_TX: [u8; TCP_TX_SIZE] = [0; TCP_TX_SIZE];
static mut DNS_QUERIES: [Option<dns::DnsQuery>; 1] = [None];
static mut HEAD: [u8; HEAD_MAX] = [0; HEAD_MAX];

/// The address configuration in use.
#[derive(Clone, Copy)]
pub struct Lease {
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub router: Option<Ipv4Addr>,
    pub dns: Option<Ipv4Addr>,
    /// "BOOTP", "DHCP" or "static".
    pub source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    Url(UrlError),
    /// The URL has a host name and no DNS server is configured.
    NoDnsServer,
    /// The name did not resolve.
    Dns,
    /// The server refused the connection or did not answer.
    Connect,
    /// No bytes arrived for 30 seconds.
    Idle,
    /// The response head is malformed or longer than 2048 bytes.
    Head,
    /// The server answered with this status.
    Status(u16),
    /// More than four redirects, or a redirect without a `Location`.
    Redirect,
    /// The connection closed before `Content-Length` bytes arrived.
    Short,
    /// The body handler stopped the transfer; it holds the reason.
    Rejected,
}

pub struct Net {
    device: &'static mut SeeqDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    tcp: SocketHandle,
    dns: Option<SocketHandle>,
    mac: [u8; 6],
    next_port: u16,
    /// Times the receive channel was restarted after an overflow.
    pub rx_restarts: u32,
    pub lease: Option<Lease>,
}

impl Net {
    /// Start the Seeq with station address `mac`.
    pub fn new(mac: [u8; 6], map: &Map) -> Result<Self, &'static str> {
        if !map.is_free(DMA_PHYS, DMA_SIZE) {
            return Err("the PROM does not report 0x09BE0000..0x09C00000 as free memory");
        }
        if core::mem::size_of::<SeeqDevice>() > DMA_SIZE as usize {
            return Err("Ethernet driver state does not fit its DMA area");
        }
        let device = SeeqDevice::new(mac).ok_or("no Seeq Ethernet controller")?;
        // SAFETY: the DMA area is free RAM that nothing else in indyboot
        // uses, addressed through kseg1.
        let device = unsafe {
            let slot = (0xA000_0000 | DMA_PHYS) as usize as *mut SeeqDevice;
            core::ptr::write(slot, device);
            &mut *slot
        };
        device.start();

        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        config.random_seed = clock::count() as u64
            ^ u64::from_be_bytes([0, 0, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]);
        let iface = Interface::new(config, device, clock::now());

        // SAFETY: `Net::new` runs once, so each static is borrowed once.
        let (storage, rx, tx) = unsafe {
            (
                &mut *core::ptr::addr_of_mut!(SOCKET_STORAGE),
                &mut *core::ptr::addr_of_mut!(TCP_RX),
                &mut *core::ptr::addr_of_mut!(TCP_TX),
            )
        };
        let mut sockets = SocketSet::new(&mut storage[..]);
        let tcp = sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(&mut rx[..]),
            tcp::SocketBuffer::new(&mut tx[..]),
        ));
        Ok(Net {
            device,
            iface,
            sockets,
            tcp,
            dns: None,
            mac,
            next_port: 49152 + (clock::count() as u16 & 0x3FFF),
            rx_restarts: 0,
            lease: None,
        })
    }

    fn poll(&mut self) {
        self.recover_rx();
        self.iface
            .poll(clock::now(), self.device, &mut self.sockets);
    }

    /// Restart the receive channel when the HPC3 has stopped it.
    ///
    /// The HPC3 stops on a receive-buffer overflow: frames arrived faster
    /// than the eight-descriptor ring was emptied. With no interrupt handler
    /// to rebuild the ring, every poll checks the channel. The frames in the
    /// ring are lost and TCP retransmits them.
    fn recover_rx(&mut self) {
        // SAFETY: RX_ENGINE is the HPC3 Ethernet receive register block.
        let control =
            unsafe { irix7_seeq::hpc3_rd32(irix7_seeq::RX_ENGINE, irix7_seeq::ENET_CTRL) };
        if control & irix7_seeq::ENET_CTRL_ACTIVE == 0 {
            self.device.restart_rx();
            self.rx_restarts += 1;
        }
    }

    /// Ask for an address with plain BOOTP.
    pub fn bootp(&mut self) -> Option<Lease> {
        let xid = clock::count()
            ^ u32::from_be_bytes([self.mac[2], self.mac[3], self.mac[4], self.mac[5]]);
        let mut request = [0u8; bootp::REQUEST_LEN];
        bootp::build_request(&mut request, self.mac, xid);
        for _ in 0..BOOTP_TRIES {
            if let Some(token) = self.device.transmit(clock::now()) {
                token.consume(request.len(), |frame| frame.copy_from_slice(&request));
            }
            let deadline = clock::now_ms() + BOOTP_WAIT_MS;
            while clock::now_ms() < deadline {
                self.recover_rx();
                let Some((rx, _tx)) = self.device.receive(clock::now()) else {
                    continue;
                };
                let mac = self.mac;
                if let Some(reply) = rx.consume(|frame| bootp::parse_reply(frame, mac, xid)) {
                    return Some(Lease {
                        address: Ipv4Addr::from(reply.address),
                        prefix: reply.prefix,
                        router: reply.router.map(Ipv4Addr::from),
                        dns: reply.dns.map(Ipv4Addr::from),
                        source: "BOOTP",
                    });
                }
            }
        }
        None
    }

    /// Ask for an address with DHCP.
    pub fn dhcp(&mut self) -> Option<Lease> {
        let handle = self.sockets.add(dhcpv4::Socket::new());
        let deadline = clock::now_ms() + DHCP_WAIT_MS;
        let mut lease = None;
        while lease.is_none() && clock::now_ms() < deadline {
            self.poll();
            if let Some(dhcpv4::Event::Configured(config)) =
                self.sockets.get_mut::<dhcpv4::Socket>(handle).poll()
            {
                lease = Some(Lease {
                    address: config.address.address(),
                    prefix: config.address.prefix_len(),
                    router: config.router,
                    dns: config.dns_servers.first().copied(),
                    source: "DHCP",
                });
            }
        }
        self.sockets.remove(handle);
        lease
    }

    /// Use `lease` for everything that follows.
    pub fn apply(&mut self, lease: Lease) {
        self.iface.update_ip_addrs(|addrs| {
            addrs.clear();
            let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(lease.address), lease.prefix));
        });
        self.iface.routes_mut().remove_default_ipv4_route();
        if let Some(router) = lease.router {
            let _ = self.iface.routes_mut().add_default_ipv4_route(router);
        }
        if let Some(server) = lease.dns {
            let servers = [IpAddress::Ipv4(server)];
            match self.dns {
                Some(handle) => {
                    self.sockets
                        .get_mut::<dns::Socket>(handle)
                        .update_servers(&servers);
                }
                None => {
                    // SAFETY: reached once; `self.dns` is set below.
                    let queries = unsafe { &mut *core::ptr::addr_of_mut!(DNS_QUERIES) };
                    self.dns = Some(
                        self.sockets
                            .add(dns::Socket::new(&servers, &mut queries[..])),
                    );
                }
            }
        }
        self.lease = Some(lease);
    }

    fn resolve(&mut self, host: &str) -> Result<Ipv4Addr, FetchError> {
        let has_server = self.lease.is_some_and(|lease| lease.dns.is_some());
        let handle = match self.dns {
            Some(handle) if has_server => handle,
            _ => return Err(FetchError::NoDnsServer),
        };
        let query = self
            .sockets
            .get_mut::<dns::Socket>(handle)
            .start_query(self.iface.context(), host, DnsQueryType::A)
            .map_err(|_| FetchError::Dns)?;
        let deadline = clock::now_ms() + DNS_WAIT_MS;
        loop {
            self.poll();
            let socket = self.sockets.get_mut::<dns::Socket>(handle);
            match socket.get_query_result(query) {
                Ok(addresses) => {
                    return addresses
                        .iter()
                        .find_map(|address| match address {
                            IpAddress::Ipv4(v4) => Some(*v4),
                            #[allow(unreachable_patterns)]
                            _ => None,
                        })
                        .ok_or(FetchError::Dns);
                }
                Err(dns::GetQueryResultError::Pending) => {}
                Err(_) => return Err(FetchError::Dns),
            }
            if clock::now_ms() > deadline {
                socket.cancel_query(query);
                return Err(FetchError::Dns);
            }
        }
    }

    /// GET `url` and pass each piece of the body to `body`, in order.
    ///
    /// Follows up to four redirects. Returns the number of body bytes.
    pub fn get(
        &mut self,
        url: &str,
        body: &mut dyn FnMut(&[u8]) -> Result<(), FetchError>,
    ) -> Result<u32, FetchError> {
        let mut current = UrlBuf::new();
        current
            .push_str(url)
            .map_err(|_| FetchError::Url(UrlError::TooLong))?;
        for _ in 0..=MAX_REDIRECTS {
            match self.get_once(&current, body)? {
                Hop::Done(bytes) => return Ok(bytes),
                Hop::Redirect(next) => current = next,
            }
        }
        Err(FetchError::Redirect)
    }

    fn get_once(
        &mut self,
        url_text: &str,
        body: &mut dyn FnMut(&[u8]) -> Result<(), FetchError>,
    ) -> Result<Hop, FetchError> {
        let url = Url::parse(url_text).map_err(FetchError::Url)?;
        let address = match url.ipv4() {
            Some(octets) => Ipv4Addr::from(octets),
            None => self.resolve(url.host)?,
        };
        let request = http::request(&url).ok_or(FetchError::Url(UrlError::TooLong))?;

        self.next_port = if self.next_port >= 65000 {
            49152
        } else {
            self.next_port + 1
        };
        let local_port = self.next_port;
        {
            let socket = self.sockets.get_mut::<tcp::Socket>(self.tcp);
            socket.abort();
            socket
                .connect(
                    self.iface.context(),
                    (IpAddress::Ipv4(address), url.port),
                    local_port,
                )
                .map_err(|_| FetchError::Connect)?;
        }
        let result = self.transfer(url_text, request.as_bytes(), body);
        // Close in both outcomes; an aborted socket sends a reset.
        let socket = self.sockets.get_mut::<tcp::Socket>(self.tcp);
        if result.is_ok() {
            socket.close();
        } else {
            socket.abort();
        }
        for _ in 0..4 {
            self.poll();
        }
        self.sockets.get_mut::<tcp::Socket>(self.tcp).abort();
        result
    }

    fn transfer(
        &mut self,
        url_text: &str,
        request: &[u8],
        body: &mut dyn FnMut(&[u8]) -> Result<(), FetchError>,
    ) -> Result<Hop, FetchError> {
        // SAFETY: one transfer runs at a time.
        let head_buffer = unsafe { &mut *core::ptr::addr_of_mut!(HEAD) };
        let mut head_have = 0usize;
        let mut sent = false;
        let mut expected: Option<u32> = None;
        let mut in_body = false;
        let mut received = 0u32;
        let mut deadline = clock::now_ms() + CONNECT_WAIT_MS;

        loop {
            self.poll();
            let socket = self.sockets.get_mut::<tcp::Socket>(self.tcp);

            if !sent {
                if socket.may_send() {
                    socket
                        .send_slice(request)
                        .map_err(|_| FetchError::Connect)?;
                    sent = true;
                    deadline = clock::now_ms() + IDLE_WAIT_MS;
                } else if !socket.is_active() {
                    return Err(FetchError::Connect);
                }
            }

            while socket.can_recv() {
                deadline = clock::now_ms() + IDLE_WAIT_MS;
                if in_body {
                    let (count, outcome) = socket
                        .recv(|data| (data.len(), (data.len(), body(data))))
                        .map_err(|_| FetchError::Short)?;
                    outcome?;
                    received += count as u32;
                    continue;
                }
                let space = HEAD_MAX - head_have;
                if space == 0 {
                    return Err(FetchError::Head);
                }
                let taken = socket
                    .recv(|data| {
                        let take = data.len().min(space);
                        head_buffer[head_have..head_have + take].copy_from_slice(&data[..take]);
                        (take, take)
                    })
                    .map_err(|_| FetchError::Short)?;
                head_have += taken;
                let head = match http::parse_head(&head_buffer[..head_have]) {
                    Ok(head) => head,
                    Err(HeadError::Incomplete) => continue,
                    Err(HeadError::Malformed) => return Err(FetchError::Head),
                };
                match head.status {
                    200 => {}
                    301 | 302 | 303 | 307 | 308 => {
                        let location = head.location.ok_or(FetchError::Redirect)?;
                        let next = url::resolve(url_text, location).map_err(FetchError::Url)?;
                        return Ok(Hop::Redirect(next));
                    }
                    status => return Err(FetchError::Status(status)),
                }
                expected = head.content_length;
                in_body = true;
                let rest = &head_buffer[head.len..head_have];
                if !rest.is_empty() {
                    body(rest)?;
                    received += rest.len() as u32;
                }
            }

            if in_body {
                if expected.is_some_and(|length| received >= length) {
                    return Ok(Hop::Done(received));
                }
                if !socket.may_recv() && !socket.can_recv() {
                    // The server closed the connection.
                    return match expected {
                        Some(_) => Err(FetchError::Short),
                        None => Ok(Hop::Done(received)),
                    };
                }
            } else if sent && !socket.may_recv() && !socket.can_recv() {
                return Err(FetchError::Head);
            }
            if clock::now_ms() > deadline {
                return Err(if sent {
                    FetchError::Idle
                } else {
                    FetchError::Connect
                });
            }
        }
    }

    /// Stop the Seeq and both HPC3 Ethernet DMA channels.
    pub fn stop(&mut self) {
        irix7_seeq::channel_reset();
    }
}

enum Hop {
    Done(u32),
    Redirect(UrlBuf),
}
