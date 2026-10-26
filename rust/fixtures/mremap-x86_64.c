/* Private anonymous resize/move preserves data, protections and fork advice. */
static long call(long n,long a,long b,long c,long d,long e,long f) {register long r10 __asm__("r10")=d;register long r8 __asm__("r8")=e;register long r9 __asm__("r9")=f;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8),"r"(r9):"rcx","r11","memory");return r;}
#define C(n,a,b,c,d) call(n,(long)(a),(long)(b),(long)(c),(long)(d),0,0)
#define CHECK(x) do {if(!(x)){C(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
__attribute__((noreturn)) void _start(void) {
    long base=call(9,0,12288,3,0x22,-1,0);CHECK(base>0);
    *(volatile long*)base=123;*(volatile long*)(base+8192)=456;
    CHECK(C(11,base+4096,4096,0,0)==0);
    CHECK(C(25,base,4096,8192,0)==base);
    CHECK(*(volatile long*)base==123 && *(volatile long*)(base+4096)==0);
    *(volatile long*)(base+4096)=789;
    CHECK(C(25,base,8192,12288,0)==-12);
    CHECK(C(25,base+1,8192,4096,0)==-22);
    CHECK(C(25,base,8192,4096,2)==-22);
    long moved=C(25,base,8192,12288,1);CHECK(moved>0 && moved!=base);
    CHECK(*(volatile long*)moved==123 && *(volatile long*)(moved+4096)==789 && *(volatile long*)(moved+8192)==0);
    CHECK(C(10,base,4096,3,0)==-12 && *(volatile long*)(base+8192)==456);
    long target=call(9,0,16384,3,0x22,-1,0);CHECK(target>0);
    CHECK(C(28,moved,12288,10,0)==0 && C(10,moved,12288,1,0)==0);
    CHECK(call(25,moved,12288,12288,3,moved+4096,0)==-22);
    CHECK(call(25,moved,12288,12288,3,target,0)==target);
    CHECK(*(volatile long*)target==123 && *(volatile long*)(target+4096)==789);
    CHECK(C(10,moved,4096,3,0)==-12);
    long child=C(57,0,0,0,0);CHECK(child>=0);
    if(child==0){CHECK(C(10,target,4096,3,0)==-12);C(231,0,0,0,0);__builtin_unreachable();}
    int status;CHECK(C(61,child,&status,0,0)==child && status==0);
    CHECK(C(28,target,12288,11,0)==0);
    child=C(57,0,0,0,0);CHECK(child>=0);
    if(child==0){*(volatile long*)target=1;C(231,99,0,0,0);__builtin_unreachable();}
    CHECK(C(61,child,&status,0,0)==child && (status&127)==11);
    CHECK(C(25,target,12288,4096,0)==target);
    CHECK(C(10,target+4096,4096,3,0)==-12 && *(volatile long*)(target+12288)==0);
    CHECK(C(11,target,16384,0,0)==0 && C(11,base+8192,4096,0,0)==0);
    C(231,0,0,0,0);__builtin_unreachable();
}
