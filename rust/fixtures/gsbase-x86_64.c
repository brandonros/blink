/* GS addressing and arch_prctl: native Linux comparison. */
static long call(long n,long a,long b,long c,long d,long e,long f) {register long r10 __asm__("r10")=d;register long r8 __asm__("r8")=e;register long r9 __asm__("r9")=f;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8),"r"(r9):"rcx","r11","memory");return r;}
#define C(n,a,b,c,d) call(n,(long)(a),(long)(b),(long)(c),(long)(d),0,0)
#define CHECK(x) do {if(!(x)){C(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
static unsigned long data[2] = {0x123456789abcdef0UL, 0};
static unsigned long get(void) { unsigned long v=0; CHECK(C(158,0x1004,&v,0,0)==0); return v; }
__attribute__((noreturn)) void _start(void) {
    CHECK(get()==0);
    CHECK(C(158,0x1001,data,0,0)==0); CHECK(get()==(unsigned long)data);
    unsigned long value;
    __asm__ volatile("movq %%gs:0,%0":"=r"(value)); CHECK(value==data[0]);
    __asm__ volatile("movq %0,%%gs:8"::"r"(value):"memory"); CHECK(data[1]==value);
    CHECK(C(158,0x1004,1,0,0)==-14); CHECK(get()==(unsigned long)data);
    CHECK(C(158,0x1001,0xffffffffffffffffUL,0,0)==-1); CHECK(get()==(unsigned long)data);
    long child=C(57,0,0,0,0); CHECK(child>=0);
    if(!child){CHECK(get()==(unsigned long)data); CHECK(C(158,0x1001,0,0,0)==0); CHECK(get()==0); C(231,0,0,0,0); __builtin_unreachable();}
    int status; CHECK(C(61,child,&status,0,0)==child && status==0); CHECK(get()==(unsigned long)data);
    CHECK(C(158,0x1001,0,0,0)==0); CHECK(get()==0);
    C(231,0,0,0,0); __builtin_unreachable();
}
