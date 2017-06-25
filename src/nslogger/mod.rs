use mio::{Events, Event, Poll} ;
use std::thread::{spawn, JoinHandle, Thread} ;
use std::thread ;
use std::sync::mpsc ;
use std::sync::{Arc, Mutex} ;
use std::vec::Vec ;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering} ;
use std::time::Duration ;
use std::path::Path ;
use std::collections::HashMap ;
use std::io::{Write, Read} ;

use tokio_core::reactor::{Core,Timeout} ;
use futures::Async ;
use futures::Stream ;
use async_dnssd ;
use async_dnssd::{Interface, BrowseResult} ;
use std::net ;
use std::net::ToSocketAddrs ;
use std::net::TcpStream ;
use openssl::ssl::{SslMethod, SslConnectorBuilder};
use openssl ;
use futures::Future ;
use futures::future::Either ;
use std::time ;

use byteorder::{BigEndian, WriteBytesExt} ;

const DEBUG_LOGGER:bool = true ;

pub enum Domain {
  App,
  View,
  Layout,
  Controller,
  Routing,
  Service,
  Network,
  Model,
  Cache,
  DB,
  IO,
  Custom(String)
}

#[derive(Copy,Clone)]
pub enum Level {
    Error,
    Warning,
    Important,
    Info,
    Debug,
    Verbose,
    Noise
}


bitflags! {
    flags LoggerOptions: u16 {
        const FLUSH_EACH_MESSAGE   = 0b00000001,
        // If set, NSLogger waits for each message to be sent to the desktop viewer (this includes connecting to the viewer)

        const BROWSE_BONJOUR       = 0b00000010,
        const USE_SSL              = 0b00000100,
        const ROUTE_TO_LOGCAT      = 0b00001000
    }
}

#[derive(Copy,Clone)]
enum LogMessageType {
    LOG = 0,               // A standard log message
    BLOCK_START,       // The start of a "block" (a group of log entries)
    BLOCK_END,         // The end of the last started "block"
    CLIENT_INFO,       // Information about the client app
    DISCONNECT,        // Pseudo-message on the desktop side to identify client disconnects
    MARK               // Pseudo-message that defines a "mark" that users can place in the log flow
}

#[derive(Debug)]
enum HandlerMessageType {
    TRY_CONNECT,
    CONNECT_COMPLETE,
    ADD_LOG(LogMessage),
    ADD_LOG_RECORD,
    OPTION_CHANGE(HashMap<String, String>),
    QUIT
}

#[derive(Copy,Clone)]
enum MessagePartKey {
    MESSAGE_TYPE  = 0,
    TIMESTAMP_S   = 1,    // "seconds" component of timestamp
    TIMESTAMP_MS  = 2,    // milliseconds component of timestamp (optional, mutually exclusive with TIMESTAMP_US)
    TIMESTAMP_US  = 3,    // microseconds component of timestamp (optional, mutually exclusive with TIMESTAMP_MS)
    THREAD_ID     = 4,
    TAG           = 5,
    LEVEL         = 6,
    MESSAGE       = 7,
    IMAGE_WIDTH   = 8,    // messages containing an image should also contain a part with the image size
    IMAGE_HEIGHT  = 9,    // (this is mainly for the desktop viewer to compute the cell size without having to immediately decode the image)
    MESSAGE_SEQ   = 10,   // the sequential number of this message which indicates the order in which messages are generated
    FILENAME      = 11,   // when logging, message can contain a file name
    LINENUMBER    = 12,   // as well as a line number
    FUNCTIONNAME  = 13,   // and a function or method name
}

#[derive(Copy,Clone)]
enum MessagePartType {
    STRING = 0,     // Strings are stored as UTF-8 data
    BINARY = 1,     // A block of binary data
    INT16 = 2,
    INT32 = 3,
    INT64 = 4,
    IMAGE = 5,      // An image, stored in PNG format
}

#[derive(Debug)]
struct LogMessage {
    pub sequence_number:u32,
    data:Vec<u8>,
    data_used:u32,
    part_count:u16
}

impl LogMessage {
    pub fn new(message_type:LogMessageType, sequence_number:u32) -> LogMessage {
        let mut new_message = LogMessage { sequence_number:sequence_number, data:Vec::with_capacity(256), data_used:6, part_count:0 } ;

        new_message.add_int32(MessagePartKey::MESSAGE_TYPE, message_type as u32) ;
        new_message.add_int32(MessagePartKey::MESSAGE_SEQ, sequence_number) ;
        new_message.add_timestamp(0) ;
        new_message.add_thread_id(thread::current().id()) ;

        new_message
    }

    pub fn add_int64(&mut self, key:MessagePartKey, value:u64) {
        self.data_used += 10 ;
        self.data.write_u8(key as u8).unwrap() ;
        self.data.write_u8(MessagePartType::INT64 as u8).unwrap() ;
        self.data.write_u64::<BigEndian>(value as u64).unwrap() ;
        self.part_count += 1 ;
    }

    pub fn add_int32(&mut self, key:MessagePartKey, value:u32) {
        self.data_used += 6 ;
        self.data.write_u8(key as u8).unwrap() ;
        self.data.write_u8(MessagePartType::INT32 as u8).unwrap() ;
        self.data.write_u32::<BigEndian>(value as u32).unwrap() ;
        self.part_count += 1 ;
    }

    pub fn add_int16(&mut self, key:MessagePartKey, value:u16) {
        self.data_used += 4 ;
        self.data.write_u8(key as u8).unwrap() ;
        self.data.write_u8(MessagePartType::INT16 as u8).unwrap() ;
        self.data.write_u16::<BigEndian>(value as u16).unwrap() ;
        self.part_count += 1 ;
    }

    fn add_bytes(&mut self, key:MessagePartKey, data_type:MessagePartType, bytes:&[u8]) {
        let length = bytes.len() ;
        self.data_used += (6 + length) as u32 ;
        self.data.write_u8(key as u8).unwrap() ;
        self.data.write_u8(data_type as u8).unwrap() ;
        self.data.write_u32::<BigEndian>(length as u32).unwrap() ;
        self.data.extend_from_slice(bytes) ;
        self.part_count += 1 ;
    }

    pub fn add_string(&mut self, key:MessagePartKey, string:&str) {
        self.add_bytes(key, MessagePartType::STRING, string.as_bytes()) ;
    }

    fn add_timestamp(&mut self, value:u64) {
        let actual_value = if value == 0 {
            let time = time::SystemTime::now().duration_since(time::UNIX_EPOCH).expect("Time went backward") ;
            (time.as_secs() * 1000 + time.subsec_nanos() as u64 / 1_000_000) as u64
        } else {
            value
        } ;

        self.add_int64(MessagePartKey::TIMESTAMP_S, actual_value / 1000) ;
        self.add_int16(MessagePartKey::TIMESTAMP_MS, (actual_value % 1000) as u16) ;
    }

    fn add_thread_id(&mut self, thread_id:thread::ThreadId) {
        // TODO

        self.add_string(MessagePartKey::THREAD_ID, "Thread ID") ;
    }

    pub fn get_bytes(&self) -> Vec<u8> {
        let mut header = Vec::with_capacity(6 + self.data.len()) ;
        let size = self.data_used - 4 ;
        header.write_u32::<BigEndian>(size) ;
        header.write_u16::<BigEndian>(self.part_count) ;

        if self.data_used == self.data.len() as u32 {
            return self.data.clone() ;
        }

        header.extend_from_slice(&self.data) ;

        return header ;
    }
}

struct LoggerState
{
    pub ready:bool,
    pub ready_waiters: Vec<Thread>,
    pub options:LoggerOptions,
    pub is_reconnection_scheduled: bool,
    pub is_connecting: bool,
    pub is_connected: bool,
    pub is_handler_running: bool,
    pub is_client_info_added: bool,
    pub bonjour_service_type: Option<String>,
    pub bonjour_service_name: Option<String>,
    /// the remote host we're talking to
    pub remote_host:Option<String>,
    pub remote_port:Option<u16>,

    /// the remote socket we're talking to
    pub remote_socket:Option<TcpStream>,

    /// file or socket output stream
    //pub write_stream:Option<Write + 'static:std::marker::Sized>,

    pub next_sequence_numbers:AtomicU32,
    pub log_messages:Vec<LogMessage>,
    pub message_sender:mpsc::Sender<HandlerMessageType>
}

impl LoggerState
{
    pub fn process_log_queue(&mut self) {
        if self.log_messages.is_empty() {
            info!(target:"NSLogger", "process_log_queue empty") ;
            return ;
        }

        if !self.is_client_info_added
        {
            self.push_client_info_to_front_of_queue() ;
        }

        // FIXME TONS OF STUFF SKIPPED!!

        if self.is_connected {
            // FIXME SKIPPING SOME OTHER STUFF

            if DEBUG_LOGGER {
                info!(target:"NSLogger", "process_log_queue: {} queued messages", self.log_messages.len()) ;
            }

            while !self.log_messages.is_empty() {
                {
                    let message = self.log_messages.first().unwrap() ;
                    info!(target:"NSLogger", "processing message {}", &message.sequence_number) ;

                    let message_vec = message.get_bytes() ;
                    let message_bytes = message_vec.as_slice() ;
                    let length = message_bytes.len() ;
                    info!(target:"NSLogger", "length: {}", length) ;
                    info!(target:"NSLogger", "bytes: {:?}", message_bytes) ;
                    let mut remaining = length ;

                    {
                        let mut tcp_stream = self.remote_socket.as_ref().unwrap() ;
                        tcp_stream.write_all(message_bytes).expect("Write to TCP stream failed") ;
                    }
                }

                self.log_messages.remove(0) ;
            }
        }

        info!(target:"NSLogger", "[{:?}] finished processing log queue", thread::current().id()) ;
    }

    fn push_client_info_to_front_of_queue(&mut self) {
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "pushing client info to front of queue") ;
        }

        let mut message = LogMessage::new(LogMessageType::CLIENT_INFO, self.next_sequence_numbers.fetch_add(1, Ordering::SeqCst)) ;
        self.log_messages.insert(0, message) ;
        self.is_client_info_added = true ;
    }

    fn change_options(&mut self, new_options:HashMap<String, String>) {

        // FIXME TEMP!!!
        self.connect_to_remote() ;
    }

    fn connect_to_remote(&mut self) -> Result<(), &str> {
        //if self.write_stream.is_some() {
            //return Err("internal error: write_stream should be none") ;
        //}
        if self.remote_socket.is_some() {
            return Err("internal error: remote_socket should be none") ;
        }

        //close_bonjour() ;

        if DEBUG_LOGGER {
            info!(target:"NSLogger", "connecting to {}:{}", self.remote_host.as_ref().unwrap(), self.remote_port.unwrap()) ;
        }

        let connect_string = format!("{}:{}", self.remote_host.as_ref().unwrap(), self.remote_port.unwrap()) ;
        let stream = match TcpStream::connect("192.168.0.8:60582") {
            Ok(s) => s,
            Err(e) => return Err("error occurred during tcp stream connection")
        } ;

        info!(target:"NSLogger", "{:?}", &stream) ;
        self.remote_socket = Some(stream) ;
        if !(self.options | USE_SSL).is_empty() {
            if DEBUG_LOGGER {
                info!(target:"NSLogger", "activating SSL connection") ;
            }

            //let mut builder = SslConnectorBuilder::new(SslMethod::tls()).unwrap() ;

            //builder.builder_mut().set_verify(openssl::ssl::SSL_VERIFY_NONE) ;

            //let connector = builder.build() ;
            //let mut stream = connector.connect(self.remote_host.as_ref().unwrap(), self.remote_socket.as_ref().unwrap()).unwrap();

            self.message_sender.send(HandlerMessageType::CONNECT_COMPLETE) ;

        }
        else {
            self.message_sender.send(HandlerMessageType::CONNECT_COMPLETE) ;
        }

        //remoteSocket = new Socket(remoteHost, remotePort);
        //if ((options & OPT_USE_SSL) != 0)
        //{
            //if (DEBUG_LOGGER)
                //Log.v("NSLogger", "activating SSL connection");

            //SSLSocketFactory sf = SSLCertificateSocketFactory.getInsecure(5000, null);
            //remoteSocket = sf.createSocket(remoteSocket, remoteHost, remotePort, true);
            //if (remoteSocket != null)
            //{
                //if (DEBUG_LOGGER)
                    //Log.v("NSLogger", String.format("starting SSL handshake with %s:%d", remoteSocket.getInetAddress().toString(), remoteSocket.getPort()));

                //SSLSocket socket = (SSLSocket) remoteSocket;
                //socket.setUseClientMode(true);
                //writeStream = remoteSocket.getOutputStream();
                //socketSendBufferSize = remoteSocket.getSendBufferSize();
                //loggingThreadHandler.sendMessage(loggingThreadHandler.obtainMessage(MSG_CONNECT_COMPLETE));
            //}
        //}
        //else
        //{
            //// non-SSL sockets are immediately ready for use
            //socketSendBufferSize = remoteSocket.getSendBufferSize();
            //writeStream = remoteSocket.getOutputStream();
            //loggingThreadHandler.sendMessage(loggingThreadHandler.obtainMessage(MSG_CONNECT_COMPLETE));
        //}
        Ok( () )
    }
}

struct MessageHandler
{
    channel_receiver:mpsc::Receiver<HandlerMessageType>,
    shared_state: Arc<Mutex<LoggerState>>,
}

impl MessageHandler {

    pub fn run_loop(&self) {
        self.shared_state.lock().unwrap().is_handler_running = true  ;
        loop {
            info!(target:"NSLogger", "[{:?}] Handler waiting for message", thread::current().id()) ;
            match self.channel_receiver.recv() {
                Ok(message) => {
                    if DEBUG_LOGGER {
                        info!(target:"NSLogger", "[{:?}] Received message: {:?}", thread::current().id(), &message) ;
                    }

                    match message {
                        HandlerMessageType::ADD_LOG(message) => {
                            if DEBUG_LOGGER {
                                info!(target:"NSLogger", "adding log {} to the queue", message.sequence_number) ;
                            }

                            let mut local_shared_state = self.shared_state.lock().unwrap() ;
                            local_shared_state.log_messages.push(message) ;
                            if local_shared_state.is_connected {
                                local_shared_state.process_log_queue() ;
                            }
                        },
                        // NOTE Depends on the LogRecord concept that seems Java-specific
                        //HandlerMessageType::ADD_LOG_RECORD => {
                            //if DEBUG_LOGGER {
                                //info!(target:"NSLogger", "adding LogRecord to the queue") ;
                            //}
                            //let mut local_shared_state = self.shared_state.lock().unwrap() ;
                            //local_shared_state.log_messages.push(LogMessage::new(
                            //if local_shared_state.is_connected {
                                //local_shared_state.process_log_queue() ;
                            //}
                        //},
                        HandlerMessageType::OPTION_CHANGE(new_options) => {
                            if DEBUG_LOGGER {
                                info!(target:"NSLogger", "options change received") ;
                            }

                            self.shared_state.lock().unwrap().change_options(new_options) ;
                        },
                        HandlerMessageType::CONNECT_COMPLETE => {
                            if DEBUG_LOGGER {
                                info!(target:"NSLogger", "connect complete message received") ;
                            }

                            let mut local_shared_state = self.shared_state.lock().unwrap() ;

                            local_shared_state.is_connecting = false ;
                            local_shared_state.is_connected = true ;

                            local_shared_state.process_log_queue() ;
                        },
                        HandlerMessageType::TRY_CONNECT => {
                            let mut local_shared_state = self.shared_state.lock().unwrap() ;
                            if DEBUG_LOGGER {
                                info!(target:"NSLogger",
                                      "try connect message received, remote socket is {:?}, connecting={:?}",
                                      local_shared_state.remote_socket,
                                      local_shared_state.is_connecting) ;
                            }

                            local_shared_state.is_reconnection_scheduled = false ;

                            if local_shared_state.remote_socket.is_none() /* && local_shared_state.write_stream.is_none() */ {
                                if !local_shared_state.is_connecting
                                        && local_shared_state.remote_host.is_some()
                                        && local_shared_state.remote_port.is_some() {
                                    local_shared_state.connect_to_remote() ;
                                }

                            }
                        },

                        HandlerMessageType::QUIT => {
                            break ;
                        }
                        _ => ()
                    }
                },
                Err(e) =>{
                    warn!(target:"NSLogger", "Error received: {:?}", e) ;
                    break ;
                }
            }
        } ;
    }
}

struct MessageWorker
{
    pub shared_state:Arc<Mutex<LoggerState>>,
    pub message_sender:mpsc::Sender<HandlerMessageType>,
    handler:MessageHandler,
}


impl MessageWorker {

    pub fn new(logger_state:Arc<Mutex<LoggerState>>, message_sender:mpsc::Sender<HandlerMessageType>, handler_receiver:mpsc::Receiver<HandlerMessageType>) -> MessageWorker {
        let state_clone = logger_state.clone() ;
        MessageWorker{ shared_state: logger_state,
                       message_sender: message_sender,
                       handler: MessageHandler{ channel_receiver: handler_receiver,
                                                shared_state:state_clone } }
    }

    fn run(&mut self) {
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "Logging thread starting up") ;
        }

        // Since we don't have a straightforward way to block the loop (cf Android), we'll setup
        // the connection before releasing the waiting thread(s).

        // Initial setup according to current parameters
        //if (bufferFile != null)
            //createBufferWriteStream();
        if { let shared_state = self.shared_state.lock().unwrap() ;
             // We're creating a local scope since a double call of lock() will systematically
             // cause a deadlock!

             shared_state.remote_host.is_some()
                && shared_state.remote_port.is_some() } {
            self.shared_state.lock().unwrap().connect_to_remote() ;
        }
        else if !(self.shared_state.lock().unwrap().options & BROWSE_BONJOUR).is_empty() {
            self.setup_bonjour() ;
        }


        // We are ready to run. Unpark the waiting threads now
        // (there may be multiple thread trying to start logging at the same time)
        self.shared_state.lock().unwrap().ready = true ;
        while !self.shared_state.lock().unwrap().ready_waiters.is_empty() {
            self.shared_state.lock().unwrap().ready_waiters.pop().unwrap().unpark() ;
        }

        if DEBUG_LOGGER {
            info!(target:"NSLogger", "Starting log event loop") ;
        }

        // Process messages
        self.handler.run_loop() ;


        if DEBUG_LOGGER {
            info!(target:"NSLogger", "Logging thread looper ended") ;
        }

        // Once loop exists, reset the variable (in case of problem we'll recreate a thread)
        //closeBonjour();
        //loggingThread = null;
        //loggingThreadHandler = null;
    }

    fn setup_bonjour(&mut self) {
        if (self.shared_state.lock().unwrap().options & BROWSE_BONJOUR).is_empty() {
            self.close_bonjour() ;
        }
        else {
            info!(target:"NSLogger", "Setting up Bonjour") ;

            let service_type = if (self.shared_state.lock().unwrap().options & USE_SSL).is_empty() {
                "_nslogger._tcp"
            } else {
                "_nslogger-ssl._tcp"
            } ;

            self.shared_state.lock().unwrap().bonjour_service_type = Some(service_type.to_string()) ;

            let mut core = Core::new().unwrap() ;
            let handle = core.handle() ;

            let mut listener = async_dnssd::browse(Interface::Any, service_type, None, &handle).unwrap() ;

            let timeout = Timeout::new(Duration::from_secs(5), &handle).unwrap() ;
            match core.run(listener.into_future().select2(timeout)) {
                Ok( either ) => {
                    match either {
                       Either::A(( ( result, browse ), _ )) => {
                           let browse_result = result.unwrap() ;
                            info!(target:"NSLogger", "Browse result: {:?}", browse_result) ;
                            info!(target:"NSLogger", "Service name: {}", browse_result.service_name) ;
                            self.shared_state.lock().unwrap().bonjour_service_name = Some(browse_result.service_name.to_string()) ;
                            match core.run(browse_result.resolve(&handle).unwrap().into_future()) {
                                Ok( (resolve_result, resolve) ) => {
                                    let resolve_details = resolve_result.unwrap() ;
                                    info!(target:"NSLogger", "Service resolution details: {:?}", resolve_details) ;
                                    for host_addr in format!("{}:{}", resolve_details.host_target, resolve_details.port).to_socket_addrs().unwrap() {


                                        if !host_addr.ip().is_global() {
                                            let ip_address = format!("{}", host_addr.ip()) ;
                                            info!(target:"NSLogger", "Bonjour host details {:?}", host_addr) ;
                                            self.shared_state.lock().unwrap().remote_host = Some(ip_address) ;
                                            self.shared_state.lock().unwrap().remote_port = Some(resolve_details.port) ;
                                            break ;
                                        }

                                    }

                                    self.message_sender.send(HandlerMessageType::TRY_CONNECT) ;
                                },
                                Err(b) => warn!(target:"NSLogger", "Couldn't resolve Bonjour service")
                            } ;
                        },
                        Either::B( ( timeout, browse ) ) => warn!(target:"NSLogger", "Bonjour discovery timed out")
                    }
                },
                Err(b) => warn!(target:"NSLogger", "Couldn't resolve Bonjour service")

            } ;
        }
    }

    fn close_bonjour(&self) {
    }
}


pub struct Logger {
    worker_thread_channel_rx: Option<mpsc::Receiver<bool>>,
    shared_state: Arc<Mutex<LoggerState>>,
    message_sender:mpsc::Sender<HandlerMessageType>,
    message_receiver:Option<mpsc::Receiver<HandlerMessageType>>,
}

impl Logger {

    pub fn new() -> Logger {
        use env_logger ;
        env_logger::init().unwrap() ;
        info!(target:"NSLogger", "NSLogger client started") ;
        let (message_sender, message_receiver) = mpsc::channel() ;
        let sender_clone = message_sender.clone() ;

        return Logger{ worker_thread_channel_rx: None,
                       message_sender: message_sender,
                       message_receiver: Some(message_receiver),
                       shared_state: Arc::new(Mutex::new(LoggerState{ options: BROWSE_BONJOUR | USE_SSL,
                                                                      ready_waiters: vec![],
                                                                      bonjour_service_type: None,
                                                                      bonjour_service_name: None,
                                                                      remote_host: None,
                                                                      remote_port: None,
                                                                      remote_socket: None,
                                                                      is_reconnection_scheduled: false,
                                                                      is_connecting: false,
                                                                      is_connected: false,
                                                                      is_handler_running: false,
                                                                      ready: false,
                                                                      is_client_info_added: false,
                                                                      next_sequence_numbers: AtomicU32::new(0),
                                                                      log_messages: vec![],
                                                                      message_sender: sender_clone
                                                                    })),
                       } ;
    }

    pub fn set_remote_host(&mut self, host_name:&str, host_port:u16, use_ssl:bool) {
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "set_remote_host host={} port={} use_ssl={}", host_name, host_port, use_ssl) ;
        }

        match self.worker_thread_channel_rx {
            Some(_) => {
                // Worker thread isn't yet setup
                let mut properties = HashMap::new() ;
                properties.insert("remote_host".to_string(), String::from(host_name)) ;
                properties.insert("remote_port".to_string(), String::from(format!("{}", host_port))) ;
                properties.insert("use_ssl".to_string(), String::from(if use_ssl { "1" } else { "0" })) ;

                self.message_sender.send(HandlerMessageType::OPTION_CHANGE(properties)) ;
            },
            None => {
                let mut local_shared_state = self.shared_state.lock().unwrap() ;
                local_shared_state.remote_host = Some(String::from(host_name)) ;
                local_shared_state.remote_port = Some(host_port) ;

                if use_ssl {
                    local_shared_state.options = local_shared_state.options | USE_SSL ;
                } else {
                    local_shared_state.options = local_shared_state.options - USE_SSL ;
                }
            }
        } ;
    }

    // FIXME Eventually take some time to fix the method dispatch issue (using macros?)!
    pub fn log_a(&mut self, filename:Option<&Path>, line_number:Option<usize>, method:Option<&str>, domain:Domain, level:Level, message:&str) {
        info!(target:"NSLogger", "entering log_a") ;
        self.start_logging_thread_if_needed() ;

        if !self.shared_state.lock().unwrap().is_handler_running {
            info!(target:"NSLogger", "Early return") ;
            return ;
        }

        info!(target:"NSLogger", "[{:?}] About to create log message", thread::current().id()) ;
        let mut log_message = LogMessage::new(LogMessageType::LOG, self.shared_state.lock().unwrap().next_sequence_numbers.fetch_add(1, Ordering::SeqCst)) ;
        log_message.add_int16(MessagePartKey::LEVEL, level as u16) ;

        log_message.add_string(MessagePartKey::MESSAGE, message) ;

        self.message_sender.send(HandlerMessageType::ADD_LOG(log_message)) ;
        info!(target:"NSLogger", "Exiting log_a") ;
    }

    pub fn log_b(&mut self, domain: Domain, level: Level, message:&str) {
        self.log_a(None, None, None, domain, level, message) ;
    }

    fn start_logging_thread_if_needed(&mut self) {
        let mut waiting = false ;

        match self.message_receiver {
            Some(_) => {
                self.shared_state.lock().unwrap().ready_waiters.push(thread::current()) ;
                let cloned_state = self.shared_state.clone() ;

                let receiver = self.message_receiver.take().unwrap() ;
                let sender = self.message_sender.clone() ;
                spawn( move || {
                    MessageWorker::new(cloned_state, sender, receiver).run() ;
                }) ;
                waiting = true ;

            },
            _ => ()

        } ;


        info!(target:"NSLogger", "Waiting for worker to be ready") ;

        while !self.shared_state.lock().unwrap().ready {
            if !waiting {
                self.shared_state.lock().unwrap().ready_waiters.push(thread::current()) ;
                waiting = true ;
            }

            thread::park_timeout(Duration::from_millis(100)) ;
            //if (Thread.interrupted())
            //   Thread.currentThread().interrupt();

        }

        info!(target:"NSLogger", "Worker is ready and running") ;
    }
}
