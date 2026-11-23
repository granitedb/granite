//! A simulated node: the [`Io`] a shard sees in the simulator, made of a
//! simulated [`Disk`], a [`Nic`] on the run's [`Fabric`](crate::network::Fabric),
//! the run's virtual [`Clock`] and a generator seeded from the run.

use std::hash::Hash;

use granite_io::memory::SplitMix64;
use granite_io::message_pool::Message;
use granite_io::{
    Completion, Connection, Full, Io, Network, Peer, Random, Storage, Tag, Time, Zone,
};

use crate::network::Nic;
use crate::{Clock, Disk, Scheduler};

pub struct Node {
    pub disk: Disk,
    pub nic: Nic,
    clock: Clock,
    random: SplitMix64,
}

impl Node {
    /// A node of `disk` and `nic`, on the clock of `sim` and with a generator
    /// seeded from it.
    pub fn new<E: Hash>(disk: Disk, nic: Nic, sim: &mut Scheduler<E>) -> Self {
        Node {
            disk,
            nic,
            clock: sim.clock(),
            random: SplitMix64(sim.next_u64()),
        }
    }
}

impl Storage for Node {
    fn size(&self, zone: Zone) -> u64 {
        self.disk.size(zone)
    }

    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.disk.read(tag, zone, offset, len, buffer)
    }

    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.disk.write(tag, zone, offset, len, buffer)
    }

    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        self.disk.fsync(tag)
    }
}

impl Network for Node {
    fn connect(&mut self, tag: Tag, to: Peer) -> Result<(), Full<()>> {
        self.nic.connect(tag, to)
    }

    fn accept(&mut self, tag: Tag) -> Result<(), Full<()>> {
        self.nic.accept(tag)
    }

    fn send(
        &mut self,
        tag: Tag,
        connection: Connection,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.nic.send(tag, connection, len, buffer)
    }

    fn recv(
        &mut self,
        tag: Tag,
        connection: Connection,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.nic.recv(tag, connection, buffer)
    }

    fn close(&mut self, connection: Connection) {
        self.nic.close(connection);
    }
}

impl Time for Node {
    fn monotonic(&self) -> u64 {
        self.clock.monotonic()
    }

    fn realtime(&self) -> i64 {
        self.clock.realtime()
    }
}

impl Random for Node {
    fn next_u64(&mut self) -> u64 {
        self.random.next_u64()
    }
}

impl Io for Node {
    /// A due completion of the disk or the network, which first drawn from
    /// the node's generator.
    fn poll(&mut self) -> Option<Completion> {
        if self.random.next_u64() & 1 == 0 {
            self.disk.poll().or_else(|| self.nic.poll())
        } else {
            self.nic.poll().or_else(|| self.disk.poll())
        }
    }
}
