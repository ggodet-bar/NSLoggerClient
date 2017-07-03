use std::io ;
use std::io::Write ;
use std::thread ;
use std::thread::Thread ;
use std::sync::mpsc ;
use std::sync::atomic::{AtomicU32, Ordering} ;
use std::net::TcpStream ;
use std::collections::HashMap ;
use std::fs::File ;
use std::io::BufWriter ;
use std::path::PathBuf ;

use openssl ;
use openssl::ssl::{SslMethod, SslConnectorBuilder, SslStream} ;

use nslogger::log_message::{LogMessage, LogMessageType} ;

use nslogger::DEBUG_LOGGER ;
use nslogger::LoggerOptions ;
use nslogger::{USE_SSL, BROWSE_BONJOUR} ;

#[derive(Debug)]
pub enum HandlerMessageType {
    TRY_CONNECT,
    CONNECT_COMPLETE,
    ADD_LOG(LogMessage),
    ADD_LOG_RECORD,
    OPTION_CHANGE(HashMap<String, String>),
    QUIT
}

#[derive(Debug)]
pub enum WriteStreamWrapper {
    Tcp(TcpStream),
    Ssl(SslStream<TcpStream>),
    File(BufWriter<File>)
}

impl WriteStreamWrapper {
    pub fn write_all(&mut self, buf:&[u8]) -> io::Result<()> {
        match *self {
            WriteStreamWrapper::Tcp(ref mut stream) => return stream.write_all(buf),
            WriteStreamWrapper::Ssl(ref mut stream) => return stream.write_all(buf),
            WriteStreamWrapper::File(ref mut stream) => return stream.write_all(buf),
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match *self {
            WriteStreamWrapper::Tcp(ref mut stream) =>  stream.flush(),
            WriteStreamWrapper::Ssl(ref mut stream) =>  stream.flush(),
            WriteStreamWrapper::File(ref mut stream) => stream.flush(),
        }
    }
}


pub struct LoggerState
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

    pub write_stream:Option<WriteStreamWrapper>,

    /// file or socket output stream
    //pub write_stream:Option<Write + 'static:std::marker::Sized>,

    next_sequence_numbers:AtomicU32,
    pub log_messages:Vec<LogMessage>,
    message_sender:mpsc::Sender<HandlerMessageType>,
    pub message_receiver:Option<mpsc::Receiver<HandlerMessageType>>,

    pub log_file_path:Option<PathBuf>,
}

impl LoggerState
{
    pub fn new(message_sender:mpsc::Sender<HandlerMessageType>, message_receiver:mpsc::Receiver<HandlerMessageType>) -> LoggerState {
        LoggerState{  options: BROWSE_BONJOUR | USE_SSL,
                      ready_waiters: vec![],
                      bonjour_service_type: None,
                      bonjour_service_name: None,
                      remote_host: None,
                      remote_port: None,
                      write_stream: None,
                      is_reconnection_scheduled: false,
                      is_connecting: false,
                      is_connected: false,
                      is_handler_running: false,
                      ready: false,
                      is_client_info_added: false,
                      next_sequence_numbers: AtomicU32::new(0),
                      log_messages: vec![],
                      message_sender: message_sender,
                      message_receiver: Some(message_receiver),
                      log_file_path: None,
        }
    }

    pub fn process_log_queue(&mut self) {
        if self.log_messages.is_empty() {
            if DEBUG_LOGGER {
                info!(target:"NSLogger", "process_log_queue empty") ;
            }
            return ;
        }

        if !self.is_client_info_added
        {
            self.push_client_info_to_front_of_queue() ;
        }

        // FIXME TONS OF STUFF SKIPPED!!

        if self.remote_host.is_none() {
            self.flush_queue_to_buffer_stream() ;
        }
        else if self.is_connected {
            // FIXME SKIPPING SOME OTHER STUFF

            self.write_messages_to_stream() ;
        }

        if DEBUG_LOGGER {
            info!(target:"NSLogger", "[{:?}] finished processing log queue", thread::current().id()) ;
        }
    }

    fn push_client_info_to_front_of_queue(&mut self) {
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "pushing client info to front of queue") ;
        }

        let message = LogMessage::new(LogMessageType::CLIENT_INFO, self.get_and_increment_sequence_number()) ;
        self.log_messages.insert(0, message) ;
        self.is_client_info_added = true ;
    }

    pub fn change_options(&mut self, new_options:HashMap<String, String>) {

        // FIXME TEMP!!!
        self.connect_to_remote() ;
    }

    pub fn connect_to_remote(&mut self) -> Result<(), &str> {
        //if self.write_stream.is_some() {
            //return Err("internal error: write_stream should be none") ;
        //}
        if self.write_stream.is_some() {
            return Err("internal error: remote_socket should be none") ;
        }

        //close_bonjour() ;

        let remote_host = self.remote_host.as_ref().unwrap() ;
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "connecting to {}:{}", remote_host, self.remote_port.unwrap()) ;
        }

        let connect_string = format!("{}:{}", remote_host, self.remote_port.unwrap()) ;
        let stream = match TcpStream::connect(connect_string) {
            Ok(s) => s,
            Err(e) => return Err("error occurred during tcp stream connection")
        } ;

        if DEBUG_LOGGER {
            info!(target:"NSLogger", "{:?}", &stream) ;
        }
        self.write_stream = Some(WriteStreamWrapper::Tcp(stream)) ;
        if !(self.options | USE_SSL).is_empty() {
            if DEBUG_LOGGER {
                info!(target:"NSLogger", "activating SSL connection") ;
            }

            let mut ssl_connector_builder = SslConnectorBuilder::new(SslMethod::tls()).unwrap() ;

            ssl_connector_builder.builder_mut().set_verify(openssl::ssl::SSL_VERIFY_NONE) ;
            ssl_connector_builder.builder_mut().set_verify_callback(openssl::ssl::SSL_VERIFY_NONE, |_,_| { true }) ;

            let connector = ssl_connector_builder.build() ;
            if let WriteStreamWrapper::Tcp(inner_stream) = self.write_stream.take().unwrap() {
                let stream = connector.danger_connect_without_providing_domain_for_certificate_verification_and_server_name_indication(inner_stream).unwrap();
                self.write_stream = Some(WriteStreamWrapper::Ssl(stream)) ;
            }

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

    pub fn get_and_increment_sequence_number(&mut self) -> u32 {
        return self.next_sequence_numbers.fetch_add(1, Ordering::SeqCst) ;
    }


    /// Write outstanding messages to the buffer file
    pub fn flush_queue_to_buffer_stream(&mut self) {
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "flush_queue_to_buffer_stream") ;
        }

        self.write_messages_to_stream() ;
    }

    fn write_messages_to_stream(&mut self) {
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "process_log_queue: {} queued messages", self.log_messages.len()) ;
        }

        while !self.log_messages.is_empty() {
            {
                let message = self.log_messages.first().unwrap() ;
                if DEBUG_LOGGER {
                    info!(target:"NSLogger", "processing message {}", &message.sequence_number) ;
                }

                let message_vec = message.get_bytes() ;
                let message_bytes = message_vec.as_slice() ;
                let length = message_bytes.len() ;
                if DEBUG_LOGGER {
                    use std::cmp ;
                    if DEBUG_LOGGER {
                        info!(target:"NSLogger", "length: {}", length) ;
                        info!(target:"NSLogger", "bytes: {:?}", &message_bytes[0..cmp::min(length, 40)]) ;
                    }
                }

                {
                    let mut tcp_stream = self.write_stream.as_mut().unwrap() ;
                    tcp_stream.write_all(message_bytes).expect("Write to stream failed") ;
                }

                match message.flush_rx {
                    None => message.flush_tx.send(true).unwrap(),
                    _ => ()
                }
            }


            self.log_messages.remove(0) ;
        }
    }
}
