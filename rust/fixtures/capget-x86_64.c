/* Capability ABI negotiation and guest-task queries, without host mutation. */
static long call(long n,long a,long b,long c,long d) {register long r10 __asm__("r10")=d;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");return r;}
#define CHECK(x) do {if(!(x)){call(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
struct header {unsigned version;int pid;};
__attribute__((noreturn)) void _start(void) {
    struct header header={0,0};unsigned data[6],other[6];
    CHECK(call(125,(long)&header,0,0,0)==0 && header.version==0x20080522);
    CHECK(call(125,(long)&header,(long)data,0,0)==0);
    header.version=0;CHECK(call(125,(long)&header,(long)other,0,0)==-22 && header.version==0x20080522);
    header.pid=call(39,0,0,0,0);CHECK(call(125,(long)&header,(long)other,0,0)==0);
    for(int n=0;n<6;n++)CHECK(data[n]==other[n]);
    header.version=0x19980330;for(int n=0;n<6;n++)other[n]=0xa5a5a5a5;
    CHECK(call(125,(long)&header,(long)other,0,0)==0);
    for(int n=0;n<3;n++)CHECK(data[n]==other[n] && other[n+3]==0xa5a5a5a5);
    header.version=0x20071026;CHECK(call(125,(long)&header,(long)other,0,0)==0);
    for(int n=0;n<6;n++)CHECK(data[n]==other[n]);
    header.version=0x20080522;header.pid=-1;CHECK(call(125,(long)&header,(long)other,0,0)==-22);
    header.pid=0x7fffffff;CHECK(call(125,(long)&header,(long)other,0,0)==-3);
    header.pid=0;CHECK(call(125,(long)&header,1,0,0)==-14 && call(125,1,(long)other,0,0)==-14);
    int pipe[2];CHECK(call(22,(long)pipe,0,0,0)==0);
    long child=call(57,0,0,0,0);CHECK(child>=0);
    if(child==0){char c;CHECK(call(0,pipe[0],(long)&c,1,0)==1);call(231,0,0,0,0);__builtin_unreachable();}
    header.pid=child;CHECK(call(125,(long)&header,(long)other,0,0)==0);
    for(int n=0;n<6;n++)CHECK(data[n]==other[n]);
    CHECK(call(1,pipe[1],(long)"x",1,0)==1);
    int status;CHECK(call(61,child,(long)&status,0,0)==child && status==0);
    call(231,0,0,0,0);__builtin_unreachable();
}
