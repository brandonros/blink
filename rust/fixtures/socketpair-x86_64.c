/* Guest-local IPC: duplex fork, descriptor sharing, readiness, EOF and SIGPIPE. */
static long call(long n,long a,long b,long c,long d) {
    register long r10 __asm__("r10")=d;
    register long r8 __asm__("r8")=0;
    register long r9 __asm__("r9")=0;
    long r;
    __asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8),"r"(r9):"rcx","r11","memory");
    return r;
}
#define CHECK(x) do { if(!(x)) { call(231,__LINE__,0,0,0); __builtin_unreachable(); } } while(0)
struct vector { const char *p; unsigned long n; };
struct pollfd { int fd; short events,revents; };
__attribute__((noreturn)) void _start(void) {
    int s[2]; char b[4096]={0};
    CHECK(call(53,1,1|0x80000|0x800,0,(long)s)==0);
    CHECK(call(72,s[0],1,0,0)==1 && call(72,s[1],1,0,0)==1);
    CHECK(call(72,s[0],3,0,0)==(2|0x800));
    long dup=call(32,s[0],0,0,0); CHECK(dup>=0 && call(72,dup,1,0,0)==0);
    CHECK(call(72,dup,4,0,0)==0 && call(72,s[0],3,0,0)==2);
    CHECK(call(72,dup,4,0x800,0)==0 && call(72,s[0],3,0,0)==(2|0x800));
    CHECK(call(0,s[0],(long)b,1,0)==-11);
    struct pollfd p={s[0],1|4|0x2000,0};
    CHECK(call(7,(long)&p,1,0,0)==1 && p.revents==4);
    struct vector vectors[]={{"abc",3},{"def",3}};
    unsigned char name[8]={0}; unsigned len=sizeof name;
    CHECK(call(51,s[0],(long)name,(long)&len,0)==0 && len==2 && name[0]==1 && name[1]==0);
    len=1; name[0]=0;
    CHECK(call(52,s[1],(long)name,(long)&len,0)==0 && len==2 && name[0]==1);
    CHECK(call(20,s[1],(long)vectors,2,0)==6);
    unsigned available=0;
    CHECK(call(16,s[0],0x541b,(long)&available,0)==0 && available==6);
    CHECK(call(45,s[0],(long)b,3,2)==3 && b[0]=='a' && b[2]=='c');
    CHECK(call(7,(long)&p,1,0,0)==1 && p.revents==(1|4));
    struct vector reads[]={{b,2},{b+2,4}};
    CHECK(call(19,s[0],(long)reads,2,0)==6 && b[0]=='a' && b[5]=='f');
    /* A bad destination must not consume the pending byte. */
    CHECK(call(1,s[1],(long)"x",1,0)==1);
    CHECK(call(45,s[0],1,1,0)==-14);
    CHECK(call(0,s[0],(long)b,1,0)==1 && b[0]=='x');
    long file=call(2,(long)"socket-sendfile",2|64|128,0600,0); CHECK(file>=0);
    CHECK(call(1,file,(long)"file",4,0)==4);
    long offset=1;
    CHECK(call(40,s[1],file,(long)&offset,3)==3 && offset==4);
    CHECK(call(0,s[0],(long)b,4,0)==3 && b[0]=='i' && b[2]=='e');
    CHECK(call(3,file,0,0,0)==0 && call(87,(long)"socket-sendfile",0,0,0)==0);
    /* Fill the bounded stream, observe backpressure, then drain it. */
    long total=0,n;
    while((n=call(1,s[0],(long)b,sizeof b,0))>0) { total+=n; CHECK(total<1048576); }
    CHECK(n==-11 && total>0);
    while(total>0) { n=call(0,s[1],(long)b,sizeof b,0); CHECK(n>0); total-=n; }
    CHECK(call(0,s[1],(long)b,1,0)==-11);
    CHECK(call(48,s[0],1,0,0)==0);
    CHECK(call(0,s[1],(long)b,1,0)==0);
    p.fd=s[1];
    CHECK(call(7,(long)&p,1,0,0)==1 && (p.revents&(1|0x2000))==(1|0x2000));
    CHECK(call(44,dup,(long)"x",1,0x4000)==-32);
    CHECK(call(44,dup,(long)"x",0,0x4000)==-32);
    CHECK(call(1,s[1],(long)"y",1,0)==1 && call(0,dup,(long)b,1,0)==1 && b[0]=='y');
    CHECK(call(3,s[0],0,0,0)==0 && call(3,dup,0,0,0)==0);
    CHECK(call(44,s[1],(long)"z",1,0x4000)==-32);
    CHECK(call(3,s[1],0,0,0)==0);
    CHECK(call(53,1,1,0,(long)s)==0);
    long child=call(57,0,0,0,0); CHECK(child>=0);
    if(child==0) {
        CHECK(call(3,s[0],0,0,0)==0);
        CHECK(call(0,s[1],(long)b,1,0)==1 && b[0]=='q');
        CHECK(call(1,s[1],(long)"r",1,0)==1);
        CHECK(call(48,s[1],1,0,0)==0);
        CHECK(call(0,s[1],(long)b,1,0)==0);
        call(231,0,0,0,0); __builtin_unreachable();
    }
    CHECK(call(3,s[1],0,0,0)==0);
    CHECK(call(1,s[0],(long)"q",1,0)==1);
    CHECK(call(0,s[0],(long)b,1,0)==1 && b[0]=='r');
    CHECK(call(0,s[0],(long)b,1,0)==0);
    CHECK(call(48,s[0],1,0,0)==0);
    int status=-1; CHECK(call(61,child,(long)&status,0,0)==child && status==0);
    CHECK(call(3,s[0],0,0,0)==0);
    call(231,0,0,0,0); __builtin_unreachable();
}
